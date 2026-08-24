use reqwest::StatusCode;
use serde_json::json;
use sqlx::{migrate::Migrator, PgPool};
use std::path::Path;
use synapse_core::{create_app, AppState};
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;

/// GET /transactions/:id now requires a resolvable tenant API key (Part A
/// fix — see TenantContext). Transactions inserted via the callback flow
/// below keep the default NULL tenant_id, which the tenant-scoped queries
/// treat as a legacy row visible to any authenticated tenant.
const TEST_API_KEY: &str = "integration-test-api-key";

async fn setup_test_app() -> (String, PgPool, impl std::any::Any) {
    setup_test_app_with_ip_filter(synapse_core::config::AllowedIps::Any, 1).await
}

async fn setup_test_app_with_ip_filter(
    allowed_ips: synapse_core::config::AllowedIps,
    trusted_proxy_depth: usize,
) -> (String, PgPool, impl std::any::Any) {
    let container = Postgres::default().start().await.unwrap();
    let host_port = container.get_host_port_ipv4(5432).await.unwrap();
    let database_url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        host_port
    );

    let pool = PgPool::connect(&database_url).await.unwrap();
    let migrator = Migrator::new(Path::join(
        Path::new(env!("CARGO_MANIFEST_DIR")),
        "migrations",
    ))
    .await
    .unwrap();
    migrator.run(&pool).await.unwrap();

    // Create partition for current month
    let _ = sqlx::query(
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
                EXECUTE format(
                    'CREATE TABLE %I PARTITION OF transactions FOR VALUES FROM (%L) TO (%L)',
                    partition_name, start_date, end_date
                );
            END IF;
        END $$;
        "#
    )
    .execute(&pool)
    .await;

    sqlx::query(
        "INSERT INTO tenants (tenant_id, name, api_key_hash, webhook_secret, stellar_account, rate_limit_per_minute, is_active) \
         VALUES ($1, 'IntegrationTestTenant', $2, pgp_sym_encrypt('', $3), '', 6000, true)",
    )
    .bind(uuid::Uuid::new_v4())
    .bind(synapse_core::db::queries::hash_api_key(TEST_API_KEY))
    .bind(synapse_core::db::queries::tenant_secret_key())
    .execute(&pool)
    .await
    .unwrap();

    let (tx, _rx) = tokio::sync::broadcast::channel(100);
    let _query_cache = synapse_core::services::QueryCache::new("redis://localhost:6379")
        .await
        .unwrap();

    let app_state = AppState {
        db: pool.clone(),
        pool_manager: synapse_core::db::pool_manager::PoolManager::new(&database_url, None, 5)
            .await
            .unwrap(),
        horizon_client: synapse_core::stellar::HorizonClient::new(
            "https://horizon-testnet.stellar.org".to_string(),
        ),
        feature_flags: synapse_core::services::feature_flags::FeatureFlagService::new(pool.clone()),
        redis_url: "redis://localhost:6379".to_string(),
        start_time: std::time::Instant::now(),
        readiness: synapse_core::ReadinessState::new(),
        tx_broadcast: tx,
        query_cache: synapse_core::services::QueryCache::new("redis://localhost:6379")
            .await
            .unwrap(),
        allowed_ips,
        trusted_proxy_depth,
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
    app_state.load_tenant_configs().await.unwrap();
    let app = create_app(app_state);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 0));
    let server = axum::Server::bind(&addr).serve(app.into_make_service());
    let actual_addr = server.local_addr();

    tokio::spawn(async move {
        server.await.unwrap();
    });

    let base_url = format!("http://{}", actual_addr);
    (base_url, pool, container)
}

#[ignore = "Requires Docker/external services"]
#[tokio::test]
async fn test_valid_deposit_flow() {
    let (base_url, _pool, _container) = setup_test_app().await;
    let client = reqwest::Client::new();

    let payload = json!({
        "stellar_account": "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "amount": "100.50",
        "asset_code": "USD",
        "callback_type": "deposit",
        "callback_status": "completed"
    });

    let res = client
        .post(format!("{}/callback", base_url))
        .header("X-App-Signature", "valid-signature")
        .json(&payload)
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::CREATED);
    let transaction: serde_json::Value = res.json().await.unwrap();
    let tx_id = transaction["id"].as_str().unwrap();

    let res = client
        .get(format!("{}/transactions/{}", base_url, tx_id))
        .header("X-API-Key", TEST_API_KEY)
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let fetched_tx: serde_json::Value = res.json().await.unwrap();
    assert_eq!(fetched_tx["id"], tx_id);
    assert!(fetched_tx["memo"].is_null());
    assert!(fetched_tx["memo_type"].is_null());
    assert!(fetched_tx["metadata"].is_null());
}

#[ignore = "Requires Docker/external services"]
#[tokio::test]
async fn test_callback_with_memo_and_metadata() {
    let (base_url, _pool, _container) = setup_test_app().await;
    let client = reqwest::Client::new();

    let payload = json!({
        "stellar_account": "GBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
        "amount": "250.00",
        "asset_code": "USDC",
        "callback_type": "deposit",
        "callback_status": "completed",
        "memo": "payment for invoice #1042",
        "memo_type": "text",
        "metadata": {
            "reference_id": "INV-1042",
            "customer_note": "Monthly subscription",
            "compliance_tag": "low_risk"
        }
    });

    let res = client
        .post(format!("{}/callback", base_url))
        .header("X-App-Signature", "valid-signature")
        .json(&payload)
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::CREATED);
    let transaction: serde_json::Value = res.json().await.unwrap();
    let tx_id = transaction["id"].as_str().unwrap();

    assert_eq!(transaction["memo"], "payment for invoice #1042");
    assert_eq!(transaction["memo_type"], "text");
    assert_eq!(transaction["metadata"]["reference_id"], "INV-1042");
    assert_eq!(
        transaction["metadata"]["customer_note"],
        "Monthly subscription"
    );
    assert_eq!(transaction["metadata"]["compliance_tag"], "low_risk");

    let res = client
        .get(format!("{}/transactions/{}", base_url, tx_id))
        .header("X-API-Key", TEST_API_KEY)
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let fetched: serde_json::Value = res.json().await.unwrap();
    assert_eq!(fetched["memo"], "payment for invoice #1042");
    assert_eq!(fetched["memo_type"], "text");
    assert_eq!(fetched["metadata"]["reference_id"], "INV-1042");
}

#[ignore = "Requires Docker/external services"]
#[tokio::test]
async fn test_callback_with_hash_memo_type() {
    let (base_url, _pool, _container) = setup_test_app().await;
    let client = reqwest::Client::new();

    let payload = json!({
        "stellar_account": "GCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC",
        "amount": "500.00",
        "asset_code": "USD",
        "memo": "abc123def456",
        "memo_type": "hash"
    });

    let res = client
        .post(format!("{}/callback", base_url))
        .header("X-App-Signature", "valid-signature")
        .json(&payload)
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::CREATED);
    let transaction: serde_json::Value = res.json().await.unwrap();
    assert_eq!(transaction["memo"], "abc123def456");
    assert_eq!(transaction["memo_type"], "hash");
}

#[ignore = "Requires Docker/external services"]
#[tokio::test]
async fn test_callback_with_invalid_memo_type() {
    let (base_url, _pool, _container) = setup_test_app().await;
    let client = reqwest::Client::new();

    let payload = json!({
        "stellar_account": "GDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD",
        "amount": "100.00",
        "asset_code": "USD",
        "memo": "some memo",
        "memo_type": "invalid_type"
    });

    let res = client
        .post(format!("{}/callback", base_url))
        .header("X-App-Signature", "valid-signature")
        .json(&payload)
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[ignore = "Requires Docker/external services"]
#[tokio::test]
async fn test_callback_with_metadata_only() {
    let (base_url, _pool, _container) = setup_test_app().await;
    let client = reqwest::Client::new();

    let payload = json!({
        "stellar_account": "GEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE",
        "amount": "75.25",
        "asset_code": "EUR",
        "metadata": {
            "partner_ref": "P-9001",
            "tags": ["recurring", "verified"]
        }
    });

    let res = client
        .post(format!("{}/callback", base_url))
        .header("X-App-Signature", "valid-signature")
        .json(&payload)
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::CREATED);
    let transaction: serde_json::Value = res.json().await.unwrap();
    assert!(transaction["memo"].is_null());
    assert!(transaction["memo_type"].is_null());
    assert_eq!(transaction["metadata"]["partner_ref"], "P-9001");
}

#[tokio::test]
#[ignore = "Signature validation not implemented"]
async fn test_invalid_signature_flow() {
    let (base_url, _pool, _container) = setup_test_app().await;
    let client = reqwest::Client::new();

    let payload = json!({
        "stellar_account": "GFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF",
        "amount": "100.50",
        "asset_code": "USD",
        "callback_type": "deposit",
        "callback_status": "completed"
    });

    let res = client
        .post(format!("{}/callback", base_url))
        .header("X-App-Signature", "invalid-signature")
        .json(&payload)
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let error_res: serde_json::Value = res.json().await.unwrap();
    assert!(error_res["error"]
        .as_str()
        .unwrap()
        .contains("Invalid signature"));
}

// ---------------------------------------------------------------------------
// IP allowlist middleware (Part C) — exercised through the real middleware
// stack via create_app(), not ip_filter.rs's unit-level tower service tests.
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "Requires Docker/external services"]
async fn test_callback_blocked_from_disallowed_ip() {
    use synapse_core::config::AllowedIps;

    let allowed = AllowedIps::Cidrs(vec!["203.0.113.0/24".parse().unwrap()]);
    // depth 0: the single X-Forwarded-For entry is trusted directly (no proxy hop).
    let (base_url, _pool, _container) = setup_test_app_with_ip_filter(allowed, 0).await;
    let client = reqwest::Client::new();

    let payload = json!({
        "stellar_account": "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "amount": "100.50",
        "asset_code": "USD",
        "callback_type": "deposit",
        "callback_status": "completed"
    });

    // Outside the allowlist -> rejected before validation/handler logic runs.
    let blocked = client
        .post(format!("{}/callback", base_url))
        .header("X-Forwarded-For", "198.51.100.55")
        .json(&payload)
        .send()
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::FORBIDDEN);

    // Inside the allowlist -> passes the IP filter (may still fail downstream
    // validation, but must not be a 403 from the filter itself).
    let allowed_resp = client
        .post(format!("{}/callback", base_url))
        .header("X-Forwarded-For", "203.0.113.10")
        .json(&payload)
        .send()
        .await
        .unwrap();
    assert_ne!(allowed_resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore = "Requires Docker/external services"]
async fn test_callback_trusted_proxy_depth_changes_trusted_entry() {
    use synapse_core::config::AllowedIps;

    // Allowlist only the real client IP, never the proxy's own IP.
    let allowed = || AllowedIps::Cidrs(vec!["203.0.113.0/24".parse().unwrap()]);
    let xff = "203.0.113.10, 198.51.100.7"; // client, then one trusted proxy hop
    let payload = json!({
        "stellar_account": "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "amount": "100.50",
        "asset_code": "USD",
        "callback_type": "deposit",
        "callback_status": "completed"
    });

    // depth = 1: correctly skips the one trusted proxy hop and reads the real
    // client entry -> passes the filter.
    let (base_url_correct, _pool1, _container1) = setup_test_app_with_ip_filter(allowed(), 1).await;
    let client = reqwest::Client::new();
    let res_correct = client
        .post(format!("{}/callback", base_url_correct))
        .header("X-Forwarded-For", xff)
        .json(&payload)
        .send()
        .await
        .unwrap();
    assert_ne!(res_correct.status(), StatusCode::FORBIDDEN);

    // depth = 0: misconfigured for this topology — trusts the proxy's own
    // (non-allowlisted) IP instead of the real client -> blocked. This proves
    // the depth setting actually changes which chain entry is authoritative,
    // not just that it compiles.
    let (base_url_wrong, _pool2, _container2) = setup_test_app_with_ip_filter(allowed(), 0).await;
    let res_wrong = client
        .post(format!("{}/callback", base_url_wrong))
        .header("X-Forwarded-For", xff)
        .json(&payload)
        .send()
        .await
        .unwrap();
    assert_eq!(res_wrong.status(), StatusCode::FORBIDDEN);
}
