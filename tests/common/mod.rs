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

use sqlx::{migrate::Migrator, PgPool};
use std::path::Path;
use synapse_core::{create_app, AppState};
use testcontainers::{runners::AsyncRunner, ImageExt};
use testcontainers_modules::postgres::Postgres;

/// GET /transactions, /transactions/:id, /transactions/search, /settlements,
/// and /settlements/:id now require a resolvable tenant API key (Part A fix
/// — see synapse_core::tenant::TenantContext). `TestApp::new` provisions a
/// single tenant with this key so existing callers of these routes keep
/// working without each test provisioning its own tenant fixture.
#[allow(dead_code)]
pub const TEST_API_KEY: &str = "common-test-app-api-key";

/// Test application with automatic database and HTTP server setup.
pub struct TestApp {
    pub base_url: String,
    pub pool: PgPool,
    pub readiness: synapse_core::ReadinessState,
    _postgres_container: Box<dyn std::any::Any>,
}

impl TestApp {
    /// Create a new test app with isolated Postgres database, migrations, and HTTP server.
    pub async fn new() -> Self {
        let container = Postgres::default()
            .with_tag("14-alpine")
            .start()
            .await
            .unwrap();
        let host_port = container.get_host_port_ipv4(5432).await.unwrap();
        let database_url = format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            host_port
        );

        let pool = PgPool::connect(&database_url).await.unwrap();

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

        sqlx::query(
            "INSERT INTO tenants (tenant_id, name, api_key, webhook_secret, stellar_account, rate_limit_per_minute, is_active) \
             VALUES ($1, 'CommonTestAppTenant', $2, '', '', 6000, true)",
        )
        .bind(uuid::Uuid::new_v4())
        .bind(TEST_API_KEY)
        .execute(&pool)
        .await
        .unwrap();

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
            redis_url: "redis://localhost:6379".to_string(),
            start_time: std::time::Instant::now(),
            readiness: synapse_core::ReadinessState::new(),
            tx_broadcast,
            query_cache: synapse_core::services::QueryCache::new("redis://localhost:6379")
                .await
                .unwrap(),
            allowed_ips: synapse_core::config::AllowedIps::Any,
            trusted_proxy_depth: 1,
            profiling_manager: synapse_core::handlers::profiling::ProfilingManager::new(),
            tenant_configs: std::sync::Arc::new(tokio::sync::RwLock::new(
                std::collections::HashMap::new(),
            )),
            pending_queue_depth: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            current_batch_size: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(10)),
            secrets_store: None,
            metrics_handle: synapse_core::metrics::init_metrics().unwrap(),
            ws_connection_pool: std::sync::Arc::new(
                synapse_core::ws::connection_pool::ConnectionPool::new(
                    synapse_core::ws::connection_pool::PoolConfig::default(),
                ),
            ),
        };

        // Clone readiness before app_state is moved into create_app
        let readiness = app_state.readiness.clone();
        app_state.load_tenant_configs().await.unwrap();

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
            readiness,
            _postgres_container: Box::new(container),
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
