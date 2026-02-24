use reqwest::StatusCode;
use serde_json::json;
use sqlx::{migrate::Migrator, PgPool};
use std::path::Path;
use synapse_core::db::pool_manager::PoolManager;
use synapse_core::services::feature_flags::FeatureFlagService;
use synapse_core::{create_app, AppState};
use tokio::net::TcpListener;

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

    let pool_manager = PoolManager::new(&database_url, None).await.unwrap();
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
