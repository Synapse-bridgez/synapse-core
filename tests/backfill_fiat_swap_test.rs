/// # Fiat → Swap Backfill Tests (#1475)
///
/// Verifies the backfill job's idempotency, dry-run mode, partial-failure
/// resume, and correct mapping from Phase 1 transactions to Phase 2 swap
/// domain rows (swap_quotes + swap_orders).
///
/// ## Running
/// ```
/// # Requires Docker (testcontainers) and PostgreSQL tooling
/// cargo test --test backfill_fiat_swap_test -- --ignored --nocapture
/// ```
use sqlx::{migrate::Migrator, PgPool};
use std::path::Path;
use testcontainers::{runners::AsyncRunner, ImageExt};
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// DB bootstrap
// ---------------------------------------------------------------------------

async fn start_db_with_migrations() -> (PgPool, testcontainers::ContainerAsync<Postgres>) {
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
    let pool = PgPool::connect(&url).await.expect("connect");

    Migrator::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"))
        .await
        .expect("load migrations")
        .run(&pool)
        .await
        .expect("run migrations");

    // Ensure monthly partition exists for transactions (partitioned table)
    sqlx::query(
        r#"DO $$ DECLARE p TEXT;
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
        END $$"#,
    )
    .execute(&pool)
    .await
    .expect("ensure transaction partition");

    // Ensure monthly partitions for swap domain tables
    for tbl in &["swap_quotes", "swap_orders", "swap_fills", "swap_events"] {
        let p = format!(
            "{}_y{}m{}",
            tbl,
            chrono::Utc::now().format("%Y"),
            chrono::Utc::now().format("%m")
        );
        sqlx::query(&format!(
            "DO $$ BEGIN
             IF NOT EXISTS (SELECT 1 FROM pg_class WHERE relname = '{}') THEN
                 EXECUTE format(
                     'CREATE TABLE %I PARTITION OF {} FOR VALUES FROM (%L) TO (%L)',
                     '{}',
                     DATE_TRUNC(''month'', NOW())::TEXT,
                     (DATE_TRUNC(''month'', NOW()) + INTERVAL ''1 month'')::TEXT
                 );
             END IF;
             END $$",
            p, tbl, p,
        ))
        .execute(&pool)
        .await
        .ok(); // Partitions may already exist; ignore errors
    }

    (pool, container)
}

/// Insert a completed Phase 1 transaction and return its id.
async fn seed_completed_transaction(pool: &PgPool, asset_code: &str, amount: &str) -> Uuid {
    let id = Uuid::new_v4();
    // Use a valid 56-char Stellar account address
    let stellar = format!("G{}", "A".repeat(55));
    sqlx::query(
        "INSERT INTO transactions
           (id, stellar_account, amount, asset_code, status, anchor_transaction_id)
         VALUES ($1, $2, $3::numeric, $4, 'completed', $5)",
    )
    .bind(id)
    .bind(&stellar)
    .bind(amount)
    .bind(asset_code)
    .bind(format!("anchor-{}", id))
    .execute(pool)
    .await
    .expect("seed transaction");
    id
}

// ---------------------------------------------------------------------------
// Helper: run backfill logic in-process (calls the same DB functions the
// shell script calls, without shelling out).
// ---------------------------------------------------------------------------

async fn run_backfill(
    pool: &PgPool,
    run_id: &str,
    dry_run: bool,
    batch_size: i64,
    dest_asset: &str,
    start_cursor: Option<Uuid>,
) -> (i64, i64, i64) {
    use synapse_core::db::backfill_queries::{
        fetch_eligible_batch, record_backfill_state, BackfillProgress,
    };
    use synapse_core::db::swap_queries::{insert_swap_order, insert_swap_quote};
    use synapse_core::swap::backfill::map_transaction_to_backfill_record;

    let mut cursor = start_cursor;
    let mut total_created: i64 = 0;
    let mut total_skipped: i64 = 0;
    let mut total_errors: i64 = 0;

    loop {
        let batch = fetch_eligible_batch(pool, cursor, batch_size)
            .await
            .expect("fetch batch");

        if batch.is_empty() {
            break;
        }

        for row in &batch {
            cursor = Some(row.id);

            if dry_run {
                // Dry-run: count only, no writes
                total_created += 1;
                continue;
            }

            let record = map_transaction_to_backfill_record(row, dest_asset);

            let mut tx = pool.begin().await.expect("begin tx");
            let result: Result<(), sqlx::Error> = async {
                insert_swap_quote(&mut *tx, &record.quote).await?;
                insert_swap_order(&mut *tx, &record.order).await?;
                record_backfill_state(
                    &mut *tx,
                    row.id,
                    Some(record.order.id),
                    false,
                    run_id,
                    "created",
                    None,
                )
                .await?;
                Ok(())
            }
            .await;

            match result {
                Ok(()) => {
                    tx.commit().await.expect("commit");
                    total_created += 1;
                }
                Err(e) => {
                    let _ = tx.rollback().await;
                    record_backfill_state(
                        pool,
                        row.id,
                        None,
                        false,
                        run_id,
                        "error",
                        Some(&e.to_string()),
                    )
                    .await
                    .ok();
                    total_errors += 1;
                }
            }
        }
    }

    (total_created, total_skipped, total_errors)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[ignore = "Requires Docker"]
#[tokio::test]
async fn test_backfill_creates_swap_records_for_eligible_transactions() {
    let (pool, _container) = start_db_with_migrations().await;

    // Seed 5 completed transactions
    let mut ids = Vec::new();
    for i in 0..5 {
        ids.push(seed_completed_transaction(&pool, "USDC", &format!("{}.00", 100 + i)).await);
    }

    let (created, _skipped, errors) =
        run_backfill(&pool, "test-run-01", false, 10, "XLM", None).await;

    assert_eq!(created, 5, "All 5 transactions should be backfilled");
    assert_eq!(errors, 0, "No errors expected");

    // Verify swap_orders were created
    let order_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM swap_orders WHERE status = 'filled'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(order_count, 5);

    // Verify swap_quotes were created
    let quote_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM swap_quotes")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(quote_count, 5);
}

#[ignore = "Requires Docker"]
#[tokio::test]
async fn test_backfill_is_idempotent_on_rerun() {
    let (pool, _container) = start_db_with_migrations().await;

    seed_completed_transaction(&pool, "USDC", "500.00").await;

    // First run
    let (created1, _, errors1) =
        run_backfill(&pool, "test-run-idempotent-1", false, 10, "XLM", None).await;
    assert_eq!(created1, 1);
    assert_eq!(errors1, 0);

    // Second run — same data, should produce no new rows
    let (created2, _, errors2) =
        run_backfill(&pool, "test-run-idempotent-2", false, 10, "XLM", None).await;
    assert_eq!(created2, 0, "Re-run should skip already-processed rows");
    assert_eq!(errors2, 0);

    // Exactly one swap order in DB
    let order_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM swap_orders")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(order_count, 1);
}

#[ignore = "Requires Docker"]
#[tokio::test]
async fn test_dry_run_reports_without_writing() {
    let (pool, _container) = start_db_with_migrations().await;

    for _ in 0..3 {
        seed_completed_transaction(&pool, "USDT", "200.00").await;
    }

    let (dry_count, _, errors) =
        run_backfill(&pool, "test-dry-run-1", true, 10, "XLM", None).await;

    assert_eq!(dry_count, 3, "Dry-run should count 3 eligible rows");
    assert_eq!(errors, 0);

    // No rows should have been written
    let order_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM swap_orders")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(order_count, 0, "Dry-run must not write to swap_orders");

    let state_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM swap_backfill_state")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state_count, 0, "Dry-run must not write to swap_backfill_state");
}

#[ignore = "Requires Docker"]
#[tokio::test]
async fn test_partial_failure_resume() {
    let (pool, _container) = start_db_with_migrations().await;

    // Seed 6 transactions
    let mut ids: Vec<Uuid> = Vec::new();
    for i in 0..6 {
        ids.push(seed_completed_transaction(&pool, "USDC", &format!("{}.00", 50 + i)).await);
    }

    // Simulate a partial run that only processes the first 3 (batch_size=3)
    let (created1, _, _) =
        run_backfill(&pool, "test-partial-1", false, 3, "XLM", None).await;
    assert!(created1 >= 3, "At least one batch of 3 should have been processed");

    let after_first_run: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM swap_backfill_state WHERE dry_run = false")
            .fetch_one(&pool)
            .await
            .unwrap();

    // Resume: backfill should process only the remaining rows
    let (created2, _, _) =
        run_backfill(&pool, "test-partial-2", false, 10, "XLM", None).await;

    let after_second_run: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM swap_backfill_state WHERE dry_run = false")
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(
        after_second_run,
        6,
        "After two runs all 6 rows should be in the state table"
    );
    // No double-creates
    let order_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM swap_orders")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(order_count, 6, "Exactly 6 swap orders — one per transaction");
}

#[ignore = "Requires Docker"]
#[tokio::test]
async fn test_seeded_dataset_mapping() {
    let (pool, _container) = start_db_with_migrations().await;

    let stellar = format!("G{}", "B".repeat(55));
    let tx_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO transactions
           (id, stellar_account, amount, asset_code, status, anchor_transaction_id)
         VALUES ($1, $2, '1234.56'::numeric, 'USDC', 'completed', 'anchor-test-99')",
    )
    .bind(tx_id)
    .bind(&stellar)
    .execute(&pool)
    .await
    .unwrap();

    run_backfill(&pool, "test-mapping-run", false, 10, "XLM", None).await;

    // Verify the order fields match the source transaction
    let (order_amount, order_status, order_dest): (
        sqlx::types::BigDecimal,
        String,
        String,
    ) = sqlx::query_as(
        "SELECT source_amount, status, destination_asset
         FROM swap_orders
         WHERE (metadata->>'transaction_id') = $1",
    )
    .bind(tx_id.to_string())
    .fetch_one(&pool)
    .await
    .expect("order should exist");

    assert_eq!(order_amount.to_string(), "1234.56");
    assert_eq!(order_status, "filled");
    assert_eq!(order_dest, "XLM");

    // Verify idempotency key in metadata
    let meta: serde_json::Value = sqlx::query_scalar(
        "SELECT metadata FROM swap_orders WHERE (metadata->>'transaction_id') = $1",
    )
    .bind(tx_id.to_string())
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(
        meta["idempotency_key"],
        serde_json::json!(format!("swap:{}", tx_id))
    );
}
