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
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;

/// Test application with automatic database and HTTP server setup.
pub struct TestApp {
    pub base_url: String,
    pub pool: PgPool,
    _postgres_container: Box<dyn std::any::Any>,
}

impl TestApp {
    /// Create a new test app with isolated Postgres database, migrations, and HTTP server.
    pub async fn new() -> Self {
        let container = Postgres::default().start().await.unwrap();
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

        // Build AppState
        let (tx_broadcast, _) = tokio::sync::broadcast::channel(100);
        let app_state = AppState {
            db: pool.clone(),
            pool_manager: synapse_core::db::pool_manager::PoolManager::new(&database_url, None)
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
            query_cache: synapse_core::services::QueryCache::new("redis://localhost:6379").unwrap(),
            profiling_manager: synapse_core::handlers::profiling::ProfilingManager::new(),
            tenant_configs: std::sync::Arc::new(tokio::sync::RwLock::new(
                std::collections::HashMap::new(),
            )),
            pending_queue_depth: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            current_batch_size: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(10)),
        };

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
            _postgres_container: Box::new(container),
        }
    }

    /// Truncate all tables for test isolation (call between tests if reusing TestApp).
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
