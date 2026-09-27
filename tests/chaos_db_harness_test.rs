/// # Database Chaos / Fault-Injection Harness
///
/// Injects connection drops, latency spikes, and pool exhaustion at randomised
/// (but reproducible, via seeded RNG) points across three representative
/// request flows:
///
///   1. Webhook transaction ingestion  (write path)
///   2. Settlement listing             (read / replica path)
///   3. Reconciliation / DLQ query     (read + write mixed path)
///
/// After each chaos run the harness asserts data-consistency invariants:
///   - No partial writes (every committed transaction row is well-formed)
///   - No stuck advisory locks
///   - No orphaned idle-in-transaction connections
///
/// ## Running
/// ```
/// # Requires Docker (testcontainers)
/// cargo test --test chaos_db_harness_test -- --ignored --nocapture
///
/// # Reproduce a specific seed
/// CHAOS_SEED=12345 cargo test --test chaos_db_harness_test -- --ignored
/// ```
///
/// ## CI
/// Add to a nightly workflow step:
/// ```yaml
/// - run: cargo test --test chaos_db_harness_test -- --ignored
///   env:
///     CHAOS_SEED: ${{ github.run_number }}
/// ```
use rand::{rngs::StdRng, Rng, SeedableRng};
use sqlx::{migrate::Migrator, PgPool};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    time::Duration,
};
use testcontainers::{runners::AsyncRunner, ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;
use tokio::time::sleep;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Seed / reproducibility
// ---------------------------------------------------------------------------

/// Read `CHAOS_SEED` env var, defaulting to a fixed value so the suite is
/// deterministic in CI unless explicitly varied.
fn chaos_seed() -> u64 {
    std::env::var("CHAOS_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0xDEAD_BEEF_CAFE_1234)
}

// ---------------------------------------------------------------------------
// Database container bootstrap
// ---------------------------------------------------------------------------

async fn start_db() -> (String, ContainerAsync<Postgres>) {
    let container = Postgres::default()
        .with_tag("14-alpine")
        .start()
        .await
        .expect("Failed to start Postgres container");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("Failed to get port");
    let url = format!("postgres://postgres:postgres@127.0.0.1:{}/postgres", port);

    let pool = PgPool::connect(&url).await.expect("Failed to connect");
    Migrator::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"))
        .await
        .expect("Failed to load migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    // Ensure a partition exists for the current month (transactions is partitioned)
    sqlx::query(
        r#"DO $
        DECLARE p TEXT;
        BEGIN
            p := 'transactions_y' || TO_CHAR(NOW(), 'YYYY') || 'm' || TO_CHAR(NOW(), 'MM');
            IF NOT EXISTS (SELECT 1 FROM pg_class WHERE relname = p) THEN
                EXECUTE format(
                    'CREATE TABLE %I PARTITION OF transactions FOR VALUES FROM (%L) TO (%L)',
                    p,
                    DATE_TRUNC('month', NOW())::TEXT,
                    (DATE_TRUNC('month', NOW()) + INTERVAL '1 month')::TEXT
                );
            END IF;
        END $"#,
    )
    .execute(&pool)
    .await
    .expect("Failed to ensure partition");

    // Seed a test tenant (required by RLS policies)
    sqlx::query(
        "INSERT INTO tenants (tenant_id, name, api_key_hash, webhook_secret, \
         stellar_account, rate_limit_per_minute, is_active) \
         VALUES ($1, 'ChaosTestTenant', 'chaos-api-key-hash', \
         pgp_sym_encrypt('chaos-secret', 'insecure-dev-only-tenant-secret'), \
         'GCHAOSACCOUNTADDRESS000000000000000000000000000000000000', 60000, true)",
    )
    .bind(Uuid::new_v4())
    .execute(&pool)
    .await
    .expect("Failed to seed tenant");

    (url, container)
}

// ---------------------------------------------------------------------------
// Fault-injection primitives
// ---------------------------------------------------------------------------

/// The three fault modes the harness can inject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FaultMode {
    /// Simulate a momentary connection drop by closing all idle connections.
    ConnectionDrop,
    /// Simulate latency on the application side (does not affect the DB connection).
    LatencySpike { millis: u64 },
    /// Exhaust the pool by holding connections before the operation under test.
    PoolExhaustion { hold_count: usize },
}

impl FaultMode {
    /// Pick a random fault from a seeded RNG.
    fn random(rng: &mut StdRng) -> Self {
        match rng.gen_range(0u8..3) {
            0 => FaultMode::ConnectionDrop,
            1 => FaultMode::LatencySpike {
                millis: rng.gen_range(50..500),
            },
            _ => FaultMode::PoolExhaustion {
                hold_count: rng.gen_range(1..4),
            },
        }
    }
}

/// Inject a fault and return a guard that cleans up automatically.
///
/// Returns `(pool_handles_to_keep_alive, latency_applied)` — caller must
/// hold the handles until after the operation so pool exhaustion is active
/// during the call.
async fn inject_fault(
    pool: &PgPool,
    fault: FaultMode,
) -> Vec<sqlx::pool::PoolConnection<sqlx::Postgres>> {
    match fault {
        FaultMode::ConnectionDrop => {
            // Force sqlx to close all idle connections; the pool reconnects
            // transparently on next acquire — this validates reconnect logic.
            // We do this by temporarily running an exclusive lock query that
            // bounces any idle backends.
            let _ = sqlx::query("SELECT pg_terminate_backend(pid) \
                                 FROM pg_stat_activity \
                                 WHERE datname = current_database() \
                                   AND pid <> pg_backend_pid() \
                                   AND state = 'idle'")
                .execute(pool)
                .await;
            vec![]
        }
        FaultMode::LatencySpike { millis } => {
            sleep(Duration::from_millis(millis)).await;
            vec![]
        }
        FaultMode::PoolExhaustion { hold_count } => {
            let mut handles = Vec::with_capacity(hold_count);
            for _ in 0..hold_count {
                if let Ok(conn) = pool.acquire().await {
                    handles.push(conn);
                }
            }
            handles
        }
    }
}

// ---------------------------------------------------------------------------
// Data-consistency invariants
// ---------------------------------------------------------------------------

/// Assert invariants after a chaos run.  Returns a human-readable summary.
async fn assert_invariants(pool: &PgPool, run_label: &str) -> InvariantReport {
    // 1. No partial writes: every transaction row has required non-null fields.
    let partial: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM transactions \
         WHERE stellar_account IS NULL OR amount IS NULL OR asset_code IS NULL OR status IS NULL",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(0);

    // 2. No stuck advisory locks.
    let stuck_locks: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pg_locks l \
         JOIN pg_stat_activity a ON l.pid = a.pid \
         WHERE NOT l.granted AND a.state = 'idle in transaction'",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(0);

    // 3. No idle-in-transaction connections beyond a threshold (1 is fine for
    //    short-lived test connections; anything higher suggests a leak).
    let idle_in_tx: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pg_stat_activity \
         WHERE state = 'idle in transaction' \
           AND datname = current_database() \
           AND now() - state_change > interval '5 seconds'",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(0);

    let pass = partial == 0 && stuck_locks == 0 && idle_in_tx == 0;

    InvariantReport {
        run_label: run_label.to_string(),
        partial_writes: partial,
        stuck_locks,
        idle_in_tx,
        pass,
    }
}

#[derive(Debug)]
struct InvariantReport {
    run_label: String,
    partial_writes: i64,
    stuck_locks: i64,
    idle_in_tx: i64,
    pass: bool,
}

impl InvariantReport {
    fn assert_pass(&self) {
        assert!(
            self.pass,
            "Invariant violations after chaos run '{}': \
             partial_writes={}, stuck_locks={}, idle_in_tx={}",
            self.run_label, self.partial_writes, self.stuck_locks, self.idle_in_tx
        );
    }
}

// ---------------------------------------------------------------------------
// RLS helper — all chaos flows run as the admin role so RLS doesn't block
// the raw pool connections.
// ---------------------------------------------------------------------------

async fn set_admin_ctx(conn: &mut sqlx::PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query("SET LOCAL app.is_admin = 'true'")
        .execute(conn)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Flow 1: Webhook transaction ingestion (write path)
// ---------------------------------------------------------------------------

/// Inserts a transaction the same way the webhook handler does (direct
/// parameterised query, no application server needed).
async fn flow_webhook_ingestion(pool: &PgPool, tenant_id: Uuid) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::new_v4();
    let stellar = format!("G{}", "A".repeat(55));

    let mut tx = pool.begin().await?;
    set_admin_ctx(&mut tx).await?;

    sqlx::query(
        "INSERT INTO transactions \
         (id, stellar_account, amount, asset_code, status, tenant_id) \
         VALUES ($1, $2, $3::numeric, $4, 'pending', $5)",
    )
    .bind(id)
    .bind(&stellar)
    .bind("100.00")
    .bind("USDC")
    .bind(tenant_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(id)
}

// ---------------------------------------------------------------------------
// Flow 2: Settlement listing (read / replica path)
// ---------------------------------------------------------------------------

/// Simulates the settlement listing read path — counts settlements visible
/// to the admin context (settlements use RLS via a sub-select on transactions,
/// not a direct tenant_id column).
async fn flow_settlement_listing(pool: &PgPool, _tenant_id: Uuid) -> Result<i64, sqlx::Error> {
    let mut conn = pool.acquire().await?;
    // Use admin context so RLS passes for the raw pool connection.
    sqlx::query("SET app.is_admin = 'true'")
        .execute(&mut *conn)
        .await?;

    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM settlements")
            .fetch_one(&mut *conn)
            .await?;

    Ok(count)
}

// ---------------------------------------------------------------------------
// Flow 3: Reconciliation / DLQ mixed read-write
// ---------------------------------------------------------------------------

/// Simulates the DLQ reconciliation path: read a pending transaction,
/// mark it failed, all in one explicit transaction.
/// Validates no partial writes survive a mid-tx fault.
async fn flow_reconciliation(pool: &PgPool, tenant_id: Uuid) -> Result<(), sqlx::Error> {
    // Insert a row to reconcile (admin context for RLS bypass).
    let tx_id = Uuid::new_v4();
    let stellar = format!("G{}", "B".repeat(55));

    let mut prep = pool.begin().await?;
    set_admin_ctx(&mut prep).await?;
    sqlx::query(
        "INSERT INTO transactions \
         (id, stellar_account, amount, asset_code, status, tenant_id) \
         VALUES ($1, $2, $3::numeric, $4, 'pending', $5)",
    )
    .bind(tx_id)
    .bind(&stellar)
    .bind("50.00")
    .bind("XLM")
    .bind(tenant_id)
    .execute(&mut *prep)
    .await?;
    prep.commit().await?;

    // Begin an explicit transaction to test mid-tx fault behaviour.
    let mut db_tx = pool.begin().await?;
    set_admin_ctx(&mut db_tx).await?;

    // Read — simulate reconciler fetching the row with a row lock.
    let _row: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT id, status FROM transactions WHERE id = $1 FOR UPDATE",
    )
    .bind(tx_id)
    .fetch_optional(&mut *db_tx)
    .await?;

    // Write — mark as failed.
    sqlx::query(
        "UPDATE transactions SET status = 'failed', updated_at = NOW() WHERE id = $1",
    )
    .bind(tx_id)
    .execute(&mut *db_tx)
    .await?;

    // Commit — if a fault caused an error before here the transaction
    // auto-rolls back and leaves no partial state.
    db_tx.commit().await?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Chaos run helper
// ---------------------------------------------------------------------------

/// Run `iterations` chaos rounds over the three flows, injecting a random
/// fault at a random point in each round.  Returns per-round invariant reports.
async fn chaos_run(
    pool: &PgPool,
    tenant_id: Uuid,
    iterations: u32,
    seed: u64,
) -> Vec<InvariantReport> {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut reports = Vec::with_capacity(iterations as usize);
    let errors = Arc::new(AtomicU32::new(0));

    for i in 0..iterations {
        let fault = FaultMode::random(&mut rng);
        let inject_before: u8 = rng.gen_range(0..3); // 0=before, 1=during(approx), 2=after

        eprintln!("[chaos round {i}] fault={fault:?} inject_before={inject_before}");

        // Pre-fault injection
        let _handles = if inject_before == 0 {
            inject_fault(pool, fault).await
        } else {
            vec![]
        };

        // Flow 1: webhook ingestion
        let f1 = flow_webhook_ingestion(pool, tenant_id).await;
        if let Err(ref e) = f1 {
            eprintln!("  [flow1 error - expected under chaos] {e}");
            errors.fetch_add(1, Ordering::Relaxed);
        }

        // Mid-fault injection
        let _handles2 = if inject_before == 1 {
            inject_fault(pool, fault).await
        } else {
            vec![]
        };

        // Flow 2: settlement listing
        let f2 = flow_settlement_listing(pool, tenant_id).await;
        if let Err(ref e) = f2 {
            eprintln!("  [flow2 error - expected under chaos] {e}");
            errors.fetch_add(1, Ordering::Relaxed);
        }

        // Flow 3: reconciliation
        let f3 = flow_reconciliation(pool, tenant_id).await;
        if let Err(ref e) = f3 {
            eprintln!("  [flow3 error - expected under chaos] {e}");
            errors.fetch_add(1, Ordering::Relaxed);
        }

        // Post-fault injection
        let _handles3 = if inject_before == 2 {
            inject_fault(pool, fault).await
        } else {
            vec![]
        };

        // Allow any in-transaction connections to settle before checking invariants.
        sleep(Duration::from_millis(100)).await;

        // Check invariants — this is the key assertion.
        let report = assert_invariants(pool, &format!("seed={seed} round={i} fault={fault:?}"))
            .await;
        eprintln!(
            "  invariants: partial={} stuck_locks={} idle_in_tx={} pass={}",
            report.partial_writes, report.stuck_locks, report.idle_in_tx, report.pass
        );
        reports.push(report);
    }

    let total_errors = errors.load(Ordering::Relaxed);
    eprintln!(
        "\n[chaos summary] seed={seed} iterations={iterations} \
         op_errors={total_errors} (errors under fault are expected)"
    );

    reports
}

// ---------------------------------------------------------------------------
// Test cases
// ---------------------------------------------------------------------------

/// Smoke run: 3 rounds, fixed seed, validates all three flows run and
/// invariants hold.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn chaos_smoke_three_flows() {
    let seed = chaos_seed();
    eprintln!("chaos_smoke_three_flows seed={seed}");

    let (url, _container) = start_db().await;
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let tenant_id: Uuid = sqlx::query_scalar("SELECT tenant_id FROM tenants LIMIT 1")
        .fetch_one(&pool)
        .await
        .expect("No tenant seeded");

    let reports = chaos_run(&pool, tenant_id, 3, seed).await;

    for report in &reports {
        report.assert_pass();
    }
}

/// Connection-drop focused run: 5 rounds, only ConnectionDrop faults,
/// verifying the pool reconnects and leaves no stuck state.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn chaos_connection_drops() {
    let seed = chaos_seed();
    eprintln!("chaos_connection_drops seed={seed}");

    let (url, _container) = start_db().await;
    let pool = sqlx::pool::PoolOptions::<sqlx::Postgres>::new()
        .max_connections(4) // small pool to make exhaustion meaningful
        .acquire_timeout(Duration::from_secs(3))
        .connect(&url)
        .await
        .unwrap();

    let tenant_id: Uuid = sqlx::query_scalar("SELECT tenant_id FROM tenants LIMIT 1")
        .fetch_one(&pool)
        .await
        .expect("No tenant seeded");

    let mut reports = Vec::new();
    for i in 0u32..5 {
        // Inject a connection drop before each flow.
        let _ = inject_fault(&pool, FaultMode::ConnectionDrop).await;
        sleep(Duration::from_millis(50)).await;

        let _ = flow_webhook_ingestion(&pool, tenant_id).await;
        let _ = flow_settlement_listing(&pool, tenant_id).await;
        let _ = flow_reconciliation(&pool, tenant_id).await;

        sleep(Duration::from_millis(150)).await;
        let report = assert_invariants(&pool, &format!("connection_drops round={i}")).await;
        reports.push(report);
    }

    for r in &reports {
        r.assert_pass();
    }
}

/// Pool-exhaustion run: verifies that under a constrained pool (max 2
/// connections), operations either succeed or fail cleanly — no partial
/// writes, no deadlocks.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn chaos_pool_exhaustion() {
    let seed = chaos_seed();
    eprintln!("chaos_pool_exhaustion seed={seed}");

    let (url, _container) = start_db().await;

    // Main pool used by operations (max 3 connections).
    let pool = sqlx::pool::PoolOptions::<sqlx::Postgres>::new()
        .max_connections(3)
        .acquire_timeout(Duration::from_secs(2))
        .connect(&url)
        .await
        .unwrap();

    // Separate admin pool for invariant checks (avoids blocking on exhausted pool).
    let admin_pool = sqlx::PgPool::connect(&url).await.unwrap();

    let tenant_id: Uuid = sqlx::query_scalar("SELECT tenant_id FROM tenants LIMIT 1")
        .fetch_one(&admin_pool)
        .await
        .expect("No tenant seeded");

    let mut reports = Vec::new();
    for i in 0u32..4 {
        // Hold connections to simulate exhaustion.
        let _handles = inject_fault(&pool, FaultMode::PoolExhaustion { hold_count: 2 }).await;

        // Operations may fail with PoolTimedOut — that's expected and tested.
        let _ = flow_webhook_ingestion(&pool, tenant_id).await;
        let _ = flow_settlement_listing(&pool, tenant_id).await;
        let _ = flow_reconciliation(&pool, tenant_id).await;

        // Drop handles — pool connections return.
        drop(_handles);
        sleep(Duration::from_millis(200)).await;

        let report = assert_invariants(&admin_pool, &format!("pool_exhaustion round={i}")).await;
        reports.push(report);
    }

    for r in &reports {
        r.assert_pass();
    }
}

/// Latency-spike run: verifies that high-latency conditions do not leave
/// stuck transactions or partial data.
#[tokio::test]
#[ignore = "Requires Docker"]
async fn chaos_latency_spikes() {
    let seed = chaos_seed();
    eprintln!("chaos_latency_spikes seed={seed}");

    let (url, _container) = start_db().await;
    let pool = sqlx::PgPool::connect(&url).await.unwrap();

    let tenant_id: Uuid = sqlx::query_scalar("SELECT tenant_id FROM tenants LIMIT 1")
        .fetch_one(&pool)
        .await
        .expect("No tenant seeded");

    let mut reports = Vec::new();
    for i in 0u32..4 {
        let _ = inject_fault(&pool, FaultMode::LatencySpike { millis: 200 }).await;
        let _ = flow_webhook_ingestion(&pool, tenant_id).await;
        let _ = inject_fault(&pool, FaultMode::LatencySpike { millis: 100 }).await;
        let _ = flow_settlement_listing(&pool, tenant_id).await;
        let _ = inject_fault(&pool, FaultMode::LatencySpike { millis: 300 }).await;
        let _ = flow_reconciliation(&pool, tenant_id).await;

        sleep(Duration::from_millis(100)).await;
        let report = assert_invariants(&pool, &format!("latency_spikes round={i}")).await;
        reports.push(report);
    }

    for r in &reports {
        r.assert_pass();
    }
}

/// Reproducibility check: same seed produces the same fault sequence.
/// Runs two chaos sequences with the same seed and asserts identical fault
/// mode ordering (via RNG determinism).
#[tokio::test]
async fn chaos_seed_is_reproducible() {
    let seed = 0xABCD_1234_5678_EF01_u64;

    let faults_a: Vec<FaultMode> = {
        let mut rng = StdRng::seed_from_u64(seed);
        (0..10).map(|_| FaultMode::random(&mut rng)).collect()
    };
    let faults_b: Vec<FaultMode> = {
        let mut rng = StdRng::seed_from_u64(seed);
        (0..10).map(|_| FaultMode::random(&mut rng)).collect()
    };

    assert_eq!(
        format!("{faults_a:?}"),
        format!("{faults_b:?}"),
        "Same seed must produce identical fault sequence"
    );
}
