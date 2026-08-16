use reqwest::StatusCode;
use serde_json::json;
use sqlx::{migrate::Migrator, PgPool};
use std::path::Path;
use std::str::FromStr;
use synapse_core::db::pool_manager::PoolManager;
use synapse_core::secrets::SecretsStore;
use synapse_core::services::feature_flags::FeatureFlagService;
use synapse_core::{create_app, AppState};
use tokio::net::TcpListener;

/// `/graphql` is mounted under `admin_router`, which requires an
/// `Authorization: Bearer <admin key>` header validated by the `admin_auth`
/// middleware. CI does not set `ADMIN_API_KEY`, so we wire a `SecretsStore`
/// into the test `AppState` (same pattern as `tests/integration_test.rs`)
/// and send this key on every request instead. `/callback` does not require
/// this header (it uses `X-API-Key`/HMAC signature auth instead).
const TEST_ADMIN_API_KEY: &str = "graphql-test-admin-key";

#[ignore = "Requires Docker/external services"]
#[tokio::test]
async fn test_graphql_queries() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(v) => v,
        Err(_) => {
            println!("Skipping GraphQL test: DATABASE_URL not set");
            return;
        }
    };

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

    let pool_manager = PoolManager::new(&database_url, None, 5).await.unwrap();
    let feature_flags = FeatureFlagService::new(pool.clone());
    let (tx_broadcast, _) = tokio::sync::broadcast::channel(100);
    let readiness = synapse_core::ReadinessState::new();
    let _query_cache = synapse_core::services::QueryCache::new("redis://localhost:6379")
        .await
        .unwrap();

    let asset_cache =
        synapse_core::AssetCache::start(pool.clone(), std::time::Duration::from_secs(300))
            .await
            .expect("failed to start asset cache in test");
    let app_state = AppState {
        db: pool.clone(),
        pool_manager,
        horizon_client: synapse_core::stellar::HorizonClient::new(
            "https://horizon-testnet.stellar.org".to_string(),
        ),
        feature_flags,
        redis_url: "redis://localhost:6379".to_string(),
        start_time: std::time::Instant::now(),
        tx_broadcast,
        readiness,
        query_cache: synapse_core::services::QueryCache::new("redis://localhost:6379")
            .await
            .unwrap(),
        profiling_manager: synapse_core::handlers::profiling::ProfilingManager::new(),
        tenant_configs: std::sync::Arc::new(tokio::sync::RwLock::new(
            std::collections::HashMap::new(),
        )),
        pending_queue_depth: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        current_batch_size: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(10)),
        secrets_store: Some(SecretsStore::new(
            "graphql-test-webhook-secret".to_string(),
            TEST_ADMIN_API_KEY.to_string(),
        )),
        metrics_handle: synapse_core::metrics::init_metrics().unwrap(),
        ws_connection_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        quota_manager: synapse_core::middleware::quota::QuotaManager::new("redis://localhost:6379")
            .expect("quota manager init failed"),
        asset_cache,
        idempotency_service: None,
    };
    let app = create_app(app_state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::Server::from_tcp(listener.into_std().unwrap())
            .unwrap()
            .serve(app.into_make_service())
            .await
            .unwrap();
    });

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::AUTHORIZATION,
        format!("Bearer {}", TEST_ADMIN_API_KEY).parse().unwrap(),
    );
    let client = reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap();
    let graphql_url = format!("http://{}/graphql", addr);

    let query = json!({
        "query": "{ transactions { id status } }"
    });
    let res = client.post(&graphql_url).json(&query).send().await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    // Seed a transaction directly via SQL rather than POST /callback: that
    // route requires X-API-Key + HMAC signature auth (a separate, unrelated
    // concern from what this test — GraphQL querying — exercises), so we
    // bypass it the same way tests/search_test.rs and tests/export_test.rs do.
    let tx_id = uuid::Uuid::new_v4();
    sqlx::query(
        r#"
        INSERT INTO transactions (id, stellar_account, amount, asset_code, status, created_at, updated_at)
        VALUES ($1, $2, $3, $4, $5, NOW(), NOW())
        "#,
    )
    .bind(tx_id)
    .bind("GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
    .bind(sqlx::types::BigDecimal::from_str("100.50").unwrap())
    .bind("USD")
    .bind("pending")
    .execute(&pool)
    .await
    .unwrap();
    let tx_id = tx_id.to_string();

    let query = json!({
        "query": format!("{{ transaction(id: \"{}\") {{ id status amount assetCode }} }}", tx_id)
    });
    let res = client.post(&graphql_url).json(&query).send().await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["data"]["transaction"]["id"], tx_id.as_str());

    // BigDecimal may have trailing zeros, so parse and compare numerically
    let amount_str = body["data"]["transaction"]["amount"].as_str().unwrap();
    let amount: f64 = amount_str.parse().unwrap();
    assert_eq!(amount, 100.50);

    assert_eq!(body["data"]["transaction"]["assetCode"], "USD");
}
