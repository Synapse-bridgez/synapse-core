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

/// `POST /callback` and `POST /webhook` now require a valid HMAC signature
/// (`middleware::webhook_signature::verify_anchor_signature`) — see
/// docs/adr/007-remove-orphaned-hexagonal-and-payments-modules.md's sibling
/// fix wiring `cache::webhook`'s signature check into the live path.
/// `TestApp::new` sets `ANCHOR_WEBHOOK_SECRET` to this value (the app has no
/// `SecretsStore` in tests, so the middleware falls back to the plain env
/// var, same as `middleware::auth::is_valid_admin_request`'s `ADMIN_API_KEY`
/// fallback) so callers can sign requests with [`sign_webhook_body`].
#[allow(dead_code)]
pub const TEST_WEBHOOK_SECRET: &str = "common-test-app-webhook-secret";

/// Signs `body` the same way `cache::webhook::verify_signature` expects:
/// HMAC-SHA256 over `{timestamp}.{body}`, keyed by [`TEST_WEBHOOK_SECRET`].
/// Returns `(timestamp, signature)` — set as the `X-Webhook-Timestamp` /
/// `X-Webhook-Signature` request headers.
#[allow(dead_code)]
pub fn sign_webhook_body(body: &[u8]) -> (String, String) {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .to_string();

    let mut mac = Hmac::<Sha256>::new_from_slice(TEST_WEBHOOK_SECRET.as_bytes()).unwrap();
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));

    (timestamp, signature)
}

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
        std::env::set_var("ANCHOR_WEBHOOK_SECRET", TEST_WEBHOOK_SECRET);

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
            "INSERT INTO tenants (tenant_id, name, api_key_hash, webhook_secret, stellar_account, rate_limit_per_minute, is_active) \
             VALUES ($1, 'CommonTestAppTenant', $2, pgp_sym_encrypt('', $3), '', 6000, true)",
        )
        .bind(uuid::Uuid::new_v4())
        .bind(synapse_core::db::queries::hash_api_key(TEST_API_KEY))
        .bind(synapse_core::db::queries::tenant_secret_key())
        .execute(&pool)
        .await
        .unwrap();

        // Build AppState via the shared fixture builder (also used by the CLI's
        // events_watch_real_server_test.rs) instead of duplicating the struct
        // literal here, so the two suites can't drift out of sync.
        let app_state = AppState::test_new(&database_url).await;

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

    /// Flip the swap subsystem into draining mode (stop accepting new orders)
    /// without touching the HTTP-level readiness flag. Used by graceful
    /// shutdown tests to assert new orders are rejected during drain.
    #[allow(dead_code)]
    pub async fn start_swap_drain(&self) {
        self.readiness.start_swap_drain();
    }

    /// Returns true once the swap subsystem is draining.
    #[allow(dead_code)]
    pub fn is_swap_draining(&self) -> bool {
        self.readiness.is_swap_draining()
    }

    /// Simulate a restart: drop the current HTTP server is out of scope for the
    /// harness, but recovery logic can be exercised by re-running the startup
    /// recovery path against the same pool. Returns the number of orders that
    /// were resumed or safely failed.
    #[allow(dead_code)]
    pub async fn run_startup_recovery(&self) -> Result<u64, sqlx::Error> {
        synapse_core::swap::recover_in_flight_orders(&self.pool).await
    }

    /// Count bridge handoff records emitted for a given swap/order id.
    #[allow(dead_code)]
    pub async fn count_handoffs_for(&self, order_id: &str) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM bridge_handoffs WHERE order_id = $1",
        )
        .bind(order_id)
        .fetch_one(&self.pool)
        .await
    }

    /// Fetch the current handoff status for a given order id, if any.
    #[allow(dead_code)]
    pub async fn handoff_status_for(&self, order_id: &str) -> Result<Option<String>, sqlx::Error> {
        sqlx::query_scalar::<_, String>(
            "SELECT status FROM bridge_handoffs WHERE order_id = $1 ORDER BY created_at DESC LIMIT 1",
        )
        .bind(order_id)
        .fetch_optional(&self.pool)
        .await
    }

    /// Drive the read-model projector to catch up on all pending outbox events.
    /// Returns the number of events processed.
    #[allow(dead_code)]
    pub async fn catch_up_read_model(&self) -> Result<u64, sqlx::Error> {
        synapse_core::swap::read_model::catch_up(&self.pool).await
    }

    /// POST a JSON body to a batch endpoint and return the parsed response.
    #[allow(dead_code)]
    pub async fn post_batch(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<(reqwest::StatusCode, serde_json::Value), Box<dyn std::error::Error>> {
        let client = reqwest::Client::new();
        let res = client
            .post(format!("{}{}", self.base_url, path))
            .header("X-API-Key", TEST_API_KEY)
            .json(body)
            .send()
            .await?;
        let status = res.status();
        let json = res.json::<serde_json::Value>().await?;
        Ok((status, json))
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
