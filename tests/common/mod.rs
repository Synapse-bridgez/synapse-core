//! Shared integration test harness with automatic database setup.
//!
//! # Usage
//! ```rust
//! use common::TestApp;
//!
//! #[tokio::test]
//! async fn my_integration_test() {
//!     let app = TestApp::new().await;
//!     let client = reqwest::Client::new();
//!     let res = client.get(format!("{}/health", app.base_url)).send().await.unwrap();
//!     assert_eq!(res.status(), 200);
//! }
//! ```
//!
//! # Test infrastructure
//!
//! `TestApp::new()` looks for Postgres in this order:
//!   1. `TEST_DATABASE_URL` — an already-running instance you provisioned yourself
//!      (e.g. `docker compose up -d postgres`, or a CI service container).
//!   2. A fresh `testcontainers` Postgres, if Docker is reachable.
//!   3. Otherwise it panics with a message naming exactly what's missing and how
//!      to fix it — no raw `testcontainers`/Docker-daemon error should ever reach
//!      the test output.
//!
//! Redis is resolved similarly via `TEST_REDIS_URL`, falling back to the default
//! `docker-compose.yml` port (`redis://127.0.0.1:6379`) rather than a testcontainer,
//! since every other integration test in this suite assumes Redis lives at that
//! fixed address (a random testcontainer port would silently desync from them).
//!
//! Reusing already-running infra (`docker compose up -d postgres redis`) avoids
//! paying the testcontainer startup cost — image pull/start + Postgres boot — on
//! every test binary, which is where most of the wall-clock savings come from
//! when running the full `tests/` suite locally or in CI.

use redis::Client as RedisClient;
use sqlx::{migrate::Migrator, PgPool};
use std::path::Path;
use std::time::Duration;
use synapse_core::{create_app, AppState};
use testcontainers::core::client::docker_client_instance;
use testcontainers::{runners::AsyncRunner, ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;

const TEST_DATABASE_URL_VAR: &str = "TEST_DATABASE_URL";
const TEST_REDIS_URL_VAR: &str = "TEST_REDIS_URL";
const DEFAULT_REDIS_URL: &str = "redis://127.0.0.1:6379";
const COMPOSE_UP_CMD: &str = "docker compose up -d postgres redis";
const DOCKER_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Test application with automatic database and HTTP server setup.
pub struct TestApp {
    pub base_url: String,
    pub pool: PgPool,
    #[allow(dead_code)]
    pub redis_url: String,
    pub readiness: synapse_core::ReadinessState,
    /// `Some` only when we started this container ourselves; `None` when we're
    /// borrowing infra the caller already had running (nothing to tear down).
    _postgres_container: Option<ContainerAsync<Postgres>>,
}

impl TestApp {
    /// Create a new test app with isolated Postgres database, migrations, and HTTP server.
    ///
    /// Reuses already-running Postgres/Redis when available (see module docs);
    /// only falls back to a fresh `testcontainers` Postgres if Docker is reachable
    /// and no `TEST_DATABASE_URL` was provided.
    pub async fn new() -> Self {
        let (pool, database_url, postgres_container) = resolve_postgres().await;
        let redis_url = resolve_redis().await;

        // Run migrations
        let migrator = Migrator::new(Path::join(
            Path::new(env!("CARGO_MANIFEST_DIR")),
            "migrations",
        ))
        .await
        .unwrap();
        migrator.run(&pool).await.unwrap();

        // Create partition for current month
        Self::create_current_partition(&pool).await;

        // Build AppState
        let (tx_broadcast, _) = tokio::sync::broadcast::channel(100);
        let app_state = AppState {
            db: pool.clone(),
            pool_manager: synapse_core::db::pool_manager::PoolManager::new(&database_url, None, 5)
                .await
                .unwrap(),
            horizon_client: synapse_core::stellar::HorizonClient::new(
                "https://horizon-testnet.stellar.org".to_string(),
            ),
            feature_flags: synapse_core::services::feature_flags::FeatureFlagService::new(
                pool.clone(),
            ),
            redis_url: redis_url.clone(),
            start_time: std::time::Instant::now(),
            readiness: synapse_core::ReadinessState::new(),
            tx_broadcast,
            query_cache: synapse_core::services::QueryCache::new(&redis_url)
                .await
                .unwrap(),
            profiling_manager: synapse_core::handlers::profiling::ProfilingManager::new(),
            tenant_configs: std::sync::Arc::new(tokio::sync::RwLock::new(
                std::collections::HashMap::new(),
            )),
            pending_queue_depth: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            current_batch_size: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(10)),
            secrets_store: None,
            metrics_handle: synapse_core::metrics::init_metrics().unwrap(),
            ws_connection_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        };

        // Clone readiness before app_state is moved into create_app
        let readiness = app_state.readiness.clone();

        let app = create_app(app_state);

        // Spawn HTTP server on random port
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 0));
        let server = axum::Server::bind(&addr).serve(app.into_make_service());
        let actual_addr = server.local_addr();

        tokio::spawn(async move {
            server.await.unwrap();
        });

        let base_url = format!("http://{}", actual_addr);

        Self {
            base_url,
            pool,
            redis_url,
            readiness,
            _postgres_container: postgres_container,
        }
    }

    /// Mark the app as ready to accept traffic.
    #[allow(dead_code)]
    pub async fn set_ready(&self) {
        self.readiness.set_ready();
    }

    /// Begin connection draining (sets not_ready + draining).
    #[allow(dead_code)]
    pub async fn start_drain(&self) {
        self.readiness.start_drain();
    }

    /// Truncate all tables for test isolation (call between tests if reusing TestApp).
    #[allow(dead_code)]
    pub async fn cleanup(&self) {
        let _ = sqlx::query("TRUNCATE TABLE transactions, settlements, audit_logs, webhook_deliveries, webhook_endpoints, transaction_dlq RESTART IDENTITY CASCADE")
            .execute(&self.pool)
            .await;
    }

    /// Create partition for the current month (required for partitioned transactions table).
    async fn create_current_partition(pool: &PgPool) {
        let _ = sqlx::query(
            r#"
            DO $
            DECLARE
                partition_date DATE;
                partition_name TEXT;
                start_date TEXT;
                end_date TEXT;
            BEGIN
                partition_date := DATE_TRUNC('month', NOW());
                partition_name := 'transactions_y' || TO_CHAR(partition_date, 'YYYY') || 'm' || TO_CHAR(partition_date, 'MM');
                start_date := TO_CHAR(partition_date, 'YYYY-MM-DD');
                end_date := TO_CHAR(partition_date + INTERVAL '1 month', 'YYYY-MM-DD');

                IF NOT EXISTS (SELECT 1 FROM pg_class WHERE relname = partition_name) THEN
                    EXECUTE format(
                        'CREATE TABLE %I PARTITION OF transactions FOR VALUES FROM (%L) TO (%L)',
                        partition_name, start_date, end_date
                    );
                END IF;
            END $;
            "#
        )
        .execute(pool)
        .await;
    }
}

/// Resolve a Postgres connection for tests, in priority order:
/// 1. `TEST_DATABASE_URL`, if set — assumed to be caller-provisioned infra.
/// 2. A fresh `testcontainers` Postgres, if Docker is reachable.
/// 3. Panic with a specific, actionable message.
async fn resolve_postgres() -> (PgPool, String, Option<ContainerAsync<Postgres>>) {
    if let Ok(url) = std::env::var(TEST_DATABASE_URL_VAR) {
        let pool = PgPool::connect(&url).await.unwrap_or_else(|err| {
            panic!(
                "{TEST_DATABASE_URL_VAR} is set to `{url}` but a connection could not be \
                 established ({err}).\n\nFix: make sure that Postgres instance is running and \
                 reachable, e.g.\n  {COMPOSE_UP_CMD}"
            )
        });
        return (pool, url, None);
    }

    if !docker_available().await {
        panic!(
            "TestApp::new() needs Postgres for integration tests and found none.\n\n\
             Checked, in order:\n  \
             1. ${TEST_DATABASE_URL_VAR} — not set\n  \
             2. Docker — daemon not reachable, so a Postgres testcontainer could not be started\n\n\
             Fix one of:\n  \
             - Start Docker, then re-run `cargo test`, or\n  \
             - Reuse the Postgres from docker-compose.yml instead of paying the testcontainer \
             startup cost on every run:\n      \
             {COMPOSE_UP_CMD}\n      \
             export {TEST_DATABASE_URL_VAR}=postgres://synapse:synapse@localhost:5432/synapse\n      \
             cargo test"
        );
    }

    let container = Postgres::default()
        .with_tag("14-alpine")
        .start()
        .await
        .unwrap_or_else(|err| {
            panic!(
                "Docker is reachable but starting a Postgres testcontainer failed ({err}).\n\n\
                 Fix: point tests at a Postgres you already have running instead:\n  \
                 {COMPOSE_UP_CMD}\n  \
                 export {TEST_DATABASE_URL_VAR}=postgres://synapse:synapse@localhost:5432/synapse"
            )
        });
    let host_port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        host_port
    );
    let pool = PgPool::connect(&url).await.unwrap();
    (pool, url, Some(container))
}

/// Resolve a Redis connection for tests, in priority order:
/// 1. `TEST_REDIS_URL`, if set — assumed to be caller-provisioned infra.
/// 2. The default docker-compose port (`redis://127.0.0.1:6379`), if something
///    answers PING there.
/// 3. Panic with a specific, actionable message.
///
/// Unlike Postgres, this does not fall back to a `testcontainers` Redis: every
/// other integration test in this suite hardcodes `redis://127.0.0.1:6379`, so a
/// container on a random port would silently desync from them rather than fail
/// loudly.
async fn resolve_redis() -> String {
    if let Ok(url) = std::env::var(TEST_REDIS_URL_VAR) {
        if let Err(err) = ping_redis(&url).await {
            panic!(
                "{TEST_REDIS_URL_VAR} is set to `{url}` but a connection could not be \
                 established ({err}).\n\nFix: make sure Redis is running and reachable, e.g.\n  \
                 {COMPOSE_UP_CMD}"
            );
        }
        return url;
    }

    if ping_redis(DEFAULT_REDIS_URL).await.is_ok() {
        return DEFAULT_REDIS_URL.to_string();
    }

    panic!(
        "TestApp::new() needs Redis for integration tests and found none.\n\n\
         Checked, in order:\n  \
         1. ${TEST_REDIS_URL_VAR} — not set\n  \
         2. {DEFAULT_REDIS_URL} — nothing answered PING\n\n\
         Fix one of:\n  \
         - Start Redis from docker-compose.yml:\n      \
         {COMPOSE_UP_CMD}\n      \
         cargo test\n  \
         - Point tests at a Redis you already have running:\n      \
         export {TEST_REDIS_URL_VAR}=redis://<host>:<port>\n      \
         cargo test"
    );
}

async fn ping_redis(url: &str) -> redis::RedisResult<()> {
    let probe = async {
        let client = RedisClient::open(url)?;
        let mut con = client.get_async_connection().await?;
        redis::cmd("PING").query_async::<_, String>(&mut con).await
    };
    match tokio::time::timeout(DOCKER_PROBE_TIMEOUT, probe).await {
        Ok(result) => result.map(|_| ()),
        Err(_) => Err(redis::RedisError::from((
            redis::ErrorKind::IoError,
            "timed out connecting to Redis",
        ))),
    }
}

/// Whether a Docker daemon is actually reachable (not just that the CLI/socket
/// path is configured) — pings it with a short timeout so a misconfigured or
/// hung `DOCKER_HOST` doesn't stall every test run.
async fn docker_available() -> bool {
    let probe = async {
        let client = docker_client_instance().await.ok()?;
        client.ping().await.ok()
    };
    matches!(
        tokio::time::timeout(DOCKER_PROBE_TIMEOUT, probe).await,
        Ok(Some(_))
    )
}
