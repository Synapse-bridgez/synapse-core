/// Regression test for the /ws resync leak described in the tracked issue:
/// any caller holding a valid API key for *any* tenant could open /ws and
/// pull recent transaction data across *every* tenant via resync, because
/// lookup_api_key returned only a bool (never which tenant matched) and the
/// resync query had no tenant filter at all — independent of the REST-route
/// auth gap and independent of RLS.
///
/// This proves the fix end-to-end against a real server: two tenants, two
/// transactions, two WebSocket connections authenticated with each tenant's
/// own API key — tenant A's resync must not contain tenant B's transaction
/// and vice versa.
use futures::{SinkExt, StreamExt};
use serde_json::Value;
use sqlx::{migrate::Migrator, PgPool};
use std::path::Path;
use synapse_core::db::pool_manager::PoolManager;
use synapse_core::handlers::ws::TransactionStatusUpdate;
use synapse_core::services::feature_flags::FeatureFlagService;
use synapse_core::{create_app, AppState};
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use uuid::Uuid;

async fn setup() -> (String, PgPool, impl std::any::Any) {
    let container = Postgres::default().start().await.unwrap();
    let host_port = container.get_host_port_ipv4(5432).await.unwrap();
    let database_url = format!("postgres://postgres:postgres@127.0.0.1:{host_port}/postgres");

    let pool = PgPool::connect(&database_url).await.unwrap();
    let migrator = Migrator::new(Path::join(
        Path::new(env!("CARGO_MANIFEST_DIR")),
        "migrations",
    ))
    .await
    .unwrap();
    migrator.run(&pool).await.unwrap();

    sqlx::query(
        r#"
        DO $$
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
                EXECUTE format('CREATE TABLE %I PARTITION OF transactions FOR VALUES FROM (%L) TO (%L)', partition_name, start_date, end_date);
            END IF;
        END $$;
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();

    let pool_manager = PoolManager::new(&database_url, None, 5).await.unwrap();
    let (tx_broadcast, _) = broadcast::channel::<TransactionStatusUpdate>(100);

    let app_state = AppState {
        db: pool.clone(),
        pool_manager,
        horizon_client: synapse_core::stellar::HorizonClient::new(
            "https://horizon-testnet.stellar.org".to_string(),
        ),
        feature_flags: FeatureFlagService::new(pool.clone()),
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

    let app = create_app(app_state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let std_listener = listener.into_std().unwrap();
    tokio::spawn(async move {
        axum::Server::from_tcp(std_listener)
            .unwrap()
            .serve(app.into_make_service())
            .await
            .unwrap();
    });

    (format!("ws://{addr}"), pool, container)
}

async fn insert_tenant_with_transaction(pool: &PgPool, name: &str, api_key: &str) -> Uuid {
    let tenant_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO tenants (tenant_id, name, api_key_hash, webhook_secret, stellar_account, rate_limit_per_minute, is_active) \
         VALUES ($1,$2,$3,pgp_sym_encrypt('', $4),'',600,true)",
    )
    .bind(tenant_id)
    .bind(name)
    .bind(synapse_core::db::queries::hash_api_key(api_key))
    .bind(synapse_core::db::queries::tenant_secret_key())
    .execute(pool)
    .await
    .unwrap();

    let tx_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO transactions (id, stellar_account, amount, asset_code, status, created_at, updated_at, tenant_id)
           VALUES ($1, 'GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA', 42, 'USD', 'pending', NOW(), NOW(), $2)"#,
    )
    .bind(tx_id)
    .bind(tenant_id)
    .execute(pool)
    .await
    .unwrap();

    tx_id
}

async fn resync_transaction_ids(base_url: &str, api_key: &str) -> Vec<Uuid> {
    let ws_url = format!("{base_url}/ws?token={api_key}");
    let (mut ws_stream, _) = connect_async(&ws_url)
        .await
        .expect("connection should succeed with a real tenant API key");

    ws_stream
        .send(Message::Text(r#"{"type":"resync","limit":20}"#.to_string()))
        .await
        .unwrap();

    // The server also sends periodic heartbeat Ping frames on this stream —
    // skip anything that isn't the resync Text response.
    let text = loop {
        let msg = tokio::time::timeout(std::time::Duration::from_secs(5), ws_stream.next())
            .await
            .expect("resync response should arrive within 5s")
            .expect("stream should not end")
            .expect("frame should not error");
        match msg {
            Message::Text(t) => break t,
            _ => continue,
        }
    };
    let parsed: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed["type"], "resync");

    parsed["events"]
        .as_array()
        .expect("events should be an array")
        .iter()
        .map(|e| Uuid::parse_str(e["id"].as_str().unwrap()).unwrap())
        .collect()
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn ws_resync_does_not_leak_across_tenants() {
    let (base_url, pool, _container) = setup().await;

    let tx_a = insert_tenant_with_transaction(&pool, "WsTenantA", "ws-tenant-a-key").await;
    let tx_b = insert_tenant_with_transaction(&pool, "WsTenantB", "ws-tenant-b-key").await;

    let tenant_a_ids = resync_transaction_ids(&base_url, "ws-tenant-a-key").await;
    assert!(
        tenant_a_ids.contains(&tx_a),
        "tenant A's resync should contain its own transaction"
    );
    assert!(
        !tenant_a_ids.contains(&tx_b),
        "tenant A's resync must not contain tenant B's transaction"
    );

    let tenant_b_ids = resync_transaction_ids(&base_url, "ws-tenant-b-key").await;
    assert!(
        tenant_b_ids.contains(&tx_b),
        "tenant B's resync should contain its own transaction"
    );
    assert!(
        !tenant_b_ids.contains(&tx_a),
        "tenant B's resync must not contain tenant A's transaction"
    );
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn ws_rejects_token_that_matches_no_admin_key_and_no_tenant() {
    let (base_url, _pool, _container) = setup().await;
    let ws_url = format!("{base_url}/ws?token=this-was-never-issued-to-anyone");
    let result = connect_async(&ws_url).await;
    assert!(
        result.is_err(),
        "a token matching neither the admin key nor any tenant's API key must be rejected \
         — before the Part A fix, ws_handler only format-validated tokens and never checked \
         them against anything real"
    );
}
