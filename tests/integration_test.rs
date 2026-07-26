use hmac::{Hmac, Mac};
use reqwest::StatusCode;
use serde_json::json;
use sha2::Sha256;
use sqlx::{migrate::Migrator, PgPool};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use synapse_core::secrets::SecretsStore;
use synapse_core::{create_app, AppState};
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;

/// Shared webhook secret used to sign requests in this test file; matches
/// the `SecretsStore` wired into the `AppState` built in `setup_test_app()`.
const TEST_WEBHOOK_SECRET: &str = "integration-test-webhook-secret";
const TEST_ADMIN_API_KEY: &str = "integration-test-admin-key";

/// Sign `body` the same way `signature_verification` middleware expects:
/// HMAC-SHA256 over `"{timestamp}.{hex(body)}"`. Returns `(timestamp, signature)`
/// as the exact header values to send.
fn sign_body(secret: &str, body: &[u8]) -> (String, String) {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .to_string();
    let signed_payload = format!("{}.{}", timestamp, hex::encode(body));

    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(signed_payload.as_bytes());
    let signature = hex::encode(mac.finalize().into_bytes());

    (timestamp, signature)
}

async fn setup_test_app() -> (String, PgPool, impl std::any::Any, String) {
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

    let (tx, _rx) = tokio::sync::broadcast::channel(100);
    let _query_cache = synapse_core::services::QueryCache::new("redis://localhost:6379")
        .await
        .unwrap();

    let asset_cache =
        synapse_core::AssetCache::start(pool.clone(), std::time::Duration::from_secs(300)).await;
    // Seed a tenant so `X-API-Key` auth (api_key_auth middleware) succeeds.
    let tenant_id = Uuid::new_v4();
    let api_key = format!("integration-test-key-{}", tenant_id);
    sqlx::query(
        "INSERT INTO tenants (tenant_id, name, api_key, webhook_secret, stellar_account, rate_limit_per_minute, is_active) VALUES ($1, $2, $3, '', '', 1000, true)"
    )
    .bind(tenant_id)
    .bind("integration-test-tenant")
    .bind(&api_key)
    .execute(&pool)
    .await
    .unwrap();

    let secrets_store = SecretsStore::new(
        TEST_WEBHOOK_SECRET.to_string(),
        TEST_ADMIN_API_KEY.to_string(),
    );

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
        profiling_manager: synapse_core::handlers::profiling::ProfilingManager::new(),
        tenant_configs: std::sync::Arc::new(tokio::sync::RwLock::new(
            std::collections::HashMap::new(),
        )),
        pending_queue_depth: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        current_batch_size: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(10)),
        secrets_store: Some(secrets_store),
        metrics_handle: synapse_core::metrics::init_metrics().unwrap(),
        ws_connection_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        quota_manager: synapse_core::middleware::quota::QuotaManager::new("redis://localhost:6379")
            .expect("quota manager init failed"),
        asset_cache,
    };
    let app = create_app(app_state);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 0));
    let server = axum::Server::bind(&addr).serve(app.into_make_service());
    let actual_addr = server.local_addr();

    tokio::spawn(async move {
        server.await.unwrap();
    });

    let base_url = format!("http://{}", actual_addr);
    (base_url, pool, container, api_key)
}

#[ignore = "Requires Docker/external services"]
#[tokio::test]
async fn test_valid_deposit_flow() {
    let (base_url, _pool, _container, api_key) = setup_test_app().await;
    let client = reqwest::Client::new();

    let payload = json!({
        "stellar_account": "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "amount": "100.50",
        "asset_code": "USD",
        "callback_type": "deposit",
        "callback_status": "completed"
    });
    let body_bytes = serde_json::to_vec(&payload).unwrap();
    let (timestamp, signature) = sign_body(TEST_WEBHOOK_SECRET, &body_bytes);

    let res = client
        .post(format!("{}/callback", base_url))
        .header("X-API-Key", &api_key)
        .header("X-Webhook-Timestamp", timestamp)
        .header("X-Webhook-Signature", signature)
        .header("Content-Type", "application/json")
        .body(body_bytes)
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::CREATED);
    let transaction: serde_json::Value = res.json().await.unwrap();
    let tx_id = transaction["id"].as_str().unwrap();

    let res = client
        .get(format!("{}/transactions/{}", base_url, tx_id))
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
    let (base_url, _pool, _container, api_key) = setup_test_app().await;
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
    let body_bytes = serde_json::to_vec(&payload).unwrap();
    let (timestamp, signature) = sign_body(TEST_WEBHOOK_SECRET, &body_bytes);

    let res = client
        .post(format!("{}/callback", base_url))
        .header("X-API-Key", &api_key)
        .header("X-Webhook-Timestamp", timestamp)
        .header("X-Webhook-Signature", signature)
        .header("Content-Type", "application/json")
        .body(body_bytes)
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
    let (base_url, _pool, _container, api_key) = setup_test_app().await;
    let client = reqwest::Client::new();

    let payload = json!({
        "stellar_account": "GCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC",
        "amount": "500.00",
        "asset_code": "USD",
        "memo": "abc123def456",
        "memo_type": "hash"
    });
    let body_bytes = serde_json::to_vec(&payload).unwrap();
    let (timestamp, signature) = sign_body(TEST_WEBHOOK_SECRET, &body_bytes);

    let res = client
        .post(format!("{}/callback", base_url))
        .header("X-API-Key", &api_key)
        .header("X-Webhook-Timestamp", timestamp)
        .header("X-Webhook-Signature", signature)
        .header("Content-Type", "application/json")
        .body(body_bytes)
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
    let (base_url, _pool, _container, api_key) = setup_test_app().await;
    let client = reqwest::Client::new();

    let payload = json!({
        "stellar_account": "GDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD",
        "amount": "100.00",
        "asset_code": "USD",
        "memo": "some memo",
        "memo_type": "invalid_type"
    });
    let body_bytes = serde_json::to_vec(&payload).unwrap();
    let (timestamp, signature) = sign_body(TEST_WEBHOOK_SECRET, &body_bytes);

    let res = client
        .post(format!("{}/callback", base_url))
        .header("X-API-Key", &api_key)
        .header("X-Webhook-Timestamp", timestamp)
        .header("X-Webhook-Signature", signature)
        .header("Content-Type", "application/json")
        .body(body_bytes)
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[ignore = "Requires Docker/external services"]
#[tokio::test]
async fn test_callback_with_metadata_only() {
    let (base_url, _pool, _container, api_key) = setup_test_app().await;
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
    let body_bytes = serde_json::to_vec(&payload).unwrap();
    let (timestamp, signature) = sign_body(TEST_WEBHOOK_SECRET, &body_bytes);

    let res = client
        .post(format!("{}/callback", base_url))
        .header("X-API-Key", &api_key)
        .header("X-Webhook-Timestamp", timestamp)
        .header("X-Webhook-Signature", signature)
        .header("Content-Type", "application/json")
        .body(body_bytes)
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::CREATED);
    let transaction: serde_json::Value = res.json().await.unwrap();
    assert!(transaction["memo"].is_null());
    assert!(transaction["memo_type"].is_null());
    assert_eq!(transaction["metadata"]["partner_ref"], "P-9001");
}

#[ignore = "Requires Docker/external services"]
#[tokio::test]
async fn test_invalid_signature_flow() {
    let (base_url, _pool, _container, api_key) = setup_test_app().await;
    let client = reqwest::Client::new();

    let payload = json!({
        "stellar_account": "GFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF",
        "amount": "100.50",
        "asset_code": "USD",
        "callback_type": "deposit",
        "callback_status": "completed"
    });
    let body_bytes = serde_json::to_vec(&payload).unwrap();
    // Sign with the wrong secret so `signature_verification` rejects the request.
    let (timestamp, _) = sign_body(TEST_WEBHOOK_SECRET, &body_bytes);
    let (_, bad_signature) = sign_body("wrong-secret", &body_bytes);

    let res = client
        .post(format!("{}/callback", base_url))
        .header("X-API-Key", &api_key)
        .header("X-Webhook-Timestamp", timestamp)
        .header("X-Webhook-Signature", bad_signature)
        .header("Content-Type", "application/json")
        .body(body_bytes)
        .send()
        .await
        .unwrap();

    // `signature_verification` middleware rejects with a bare 401 (no JSON body) —
    // it runs before any handler-level `AppError` JSON formatting applies.
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}
