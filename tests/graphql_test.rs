use reqwest::StatusCode;
use serde_json::json;
use sqlx::{migrate::Migrator, PgPool};
use std::path::Path;
use synapse_core::db::pool_manager::PoolManager;
use synapse_core::services::feature_flags::FeatureFlagService;
use synapse_core::{create_app, AppState};
use tokio::net::TcpListener;

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

    tokio::spawn(async move {
        axum::Server::from_tcp(listener.into_std().unwrap())
            .unwrap()
            .serve(app.into_make_service())
            .await
            .unwrap();
    });

    let client = reqwest::Client::new();
    let graphql_url = format!("http://{}/graphql", addr);

    let query = json!({
        "query": "{ transactions { id status } }"
    });
    let res = client.post(&graphql_url).json(&query).send().await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let callback_url = format!("http://{}/callback", addr);
    let payload = json!({
        "stellar_account": "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "amount": "100.50",
        "asset_code": "USD",
        "callback_type": "deposit",
        "callback_status": "completed"
    });
    let res = client
        .post(&callback_url)
        .json(&payload)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let tx: serde_json::Value = res.json().await.unwrap();
    let tx_id = tx["id"].as_str().unwrap();

    let query = json!({
        "query": format!("{{ transaction(id: \"{}\") {{ id status amount assetCode }} }}", tx_id)
    });
    let res = client.post(&graphql_url).json(&query).send().await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["data"]["transaction"]["id"], tx_id);

    // BigDecimal may have trailing zeros, so parse and compare numerically
    let amount_str = body["data"]["transaction"]["amount"].as_str().unwrap();
    let amount: f64 = amount_str.parse().unwrap();
    assert_eq!(amount, 100.50);

    assert_eq!(body["data"]["transaction"]["assetCode"], "USD");
}

// ---------------------------------------------------------------------------
// GraphQL complexity scoring (Part E). NOTE: the live `/graphql` HTTP route
// (handlers::graphql::graphql_handler) is a hand-rolled string-matching stand-in
// that never calls into the real async-graphql schema (see issue #7) — so this
// test executes against `AppSchema` directly via `Schema::execute`, which is
// the only currently-reachable path that exercises the resolver-level
// `#[graphql(complexity = ...)]` annotations this change adds.
// ---------------------------------------------------------------------------

#[ignore = "Requires Docker/external services"]
#[tokio::test]
async fn test_graphql_complexity_scales_with_requested_limit() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(v) => v,
        Err(_) => {
            println!("Skipping GraphQL complexity test: DATABASE_URL not set");
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

    let pool_manager = PoolManager::new(&database_url, None, 5).await.unwrap();
    let feature_flags = FeatureFlagService::new(pool.clone());
    let (tx_broadcast, _) = tokio::sync::broadcast::channel(100);
    let readiness = synapse_core::ReadinessState::new();

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

    let schema = synapse_core::graphql::schema::build_schema(app_state);

    // Exactly at the alias cap (20), each requesting the max page size (1000).
    // Under the old field-occurrence-only accounting this stayed well under
    // MAX_QUERY_COMPLEXITY (1000) despite up to 20,000 row-equivalent work;
    // with limit-scaled complexity it must now be rejected.
    let aliased_fields: String = (0..20)
        .map(|i| format!("a{i}: transactions(limit: 1000) {{ id }}"))
        .collect::<Vec<_>>()
        .join(" ");
    let exploit_query = format!("{{ {aliased_fields} }}");

    let response = schema
        .execute(async_graphql::Request::new(exploit_query))
        .await;
    assert!(
        !response.errors.is_empty(),
        "20 aliases x limit:1000 should be rejected by the complexity limit, got: {:?}",
        response.data
    );
    assert!(
        response
            .errors
            .iter()
            .any(|e| e.message.to_lowercase().contains("complex")),
        "expected a complexity-limit error, got: {:?}",
        response.errors
    );

    // A single modest-limit query must still pass, proving this isn't a
    // blanket rejection.
    let modest_response = schema
        .execute(async_graphql::Request::new(
            "{ transactions(limit: 20) { id } }".to_string(),
        ))
        .await;
    assert!(
        modest_response.errors.is_empty(),
        "modest query should not trip the complexity limit: {:?}",
        modest_response.errors
    );
}
