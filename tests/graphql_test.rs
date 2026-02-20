use synapse_core::{create_app, AppState};
use testcontainers_modules::postgres::Postgres;
use testcontainers::runners::AsyncRunner;
use sqlx::{PgPool, migrate::Migrator};
use std::path::Path;
use tokio::net::TcpListener;
use reqwest::StatusCode;
use serde_json::json;

#[tokio::test]
async fn test_graphql_queries() {
    let container = Postgres::default().start().await.unwrap();
    let host_port = container.get_host_port_ipv4(5432).await.unwrap();
    let database_url = format!("postgres://postgres:postgres@127.0.0.1:{}/postgres", host_port);

    // Run migrations
    let pool = PgPool::connect(&database_url).await.unwrap();
    let migrator = Migrator::new(Path::join(Path::new(env!("CARGO_MANIFEST_DIR")), "migrations")).await.unwrap();
    migrator.run(&pool).await.unwrap();

    // Start App
    let app_state = AppState {
        db: pool.clone(),
        horizon_client: synapse_core::stellar::HorizonClient::new("https://horizon-testnet.stellar.org".to_string()),
    };
    let app = create_app(app_state);
    
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = reqwest::Client::new();
    let graphql_url = format!("http://{}/graphql", addr);

    // 1. Query empty transactions
    let query = json!({
        "query": "{ transactions { id status } }"
    });
    let res = client.post(&graphql_url).json(&query).send().await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: serde_json::Value = res.json().await.unwrap();
    assert!(body["data"]["transactions"].as_array().unwrap().is_empty());

    // 2. Insert a transaction via REST callback
    let callback_url = format!("http://{}/callback", addr);
    let payload = json!({
        "stellar_account": "GABC1234567890",
        "amount": "100.50",
        "asset_code": "USD",
        "callback_type": "deposit",
        "callback_status": "completed"
    });
    let res = client.post(&callback_url)
        .header("X-App-Signature", "valid-signature")
        .json(&payload)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let tx: serde_json::Value = res.json().await.unwrap();
    let tx_id = tx["id"].as_str().unwrap();

    // 3. Query the inserted transaction via GraphQL
    let query = json!({
        "query": format!("{{ transaction(id: \"{}\") {{ id status amount assetCode }} }}", tx_id)
    });
    let res = client.post(&graphql_url).json(&query).send().await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["data"]["transaction"]["id"], tx_id);
    assert_eq!(body["data"]["transaction"]["amount"], "100.50");
    assert_eq!(body["data"]["transaction"]["assetCode"], "USD");

    // 4. Test filtering
    let query = json!({
        "query": "query($filter: TransactionFilter) { transactions(filter: $filter) { id status } }",
        "variables": {
            "filter": {
                "status": "pending"
            }
        }
    });
    let res = client.post(&graphql_url).json(&query).send().await.unwrap();
    let body: serde_json::Value = res.json().await.unwrap();
    // Default status from callback handler is 'pending' if not specified, 
    // but the handler in src/handlers/webhook.rs seems to set it to 'pending' by default.
    // Let's verify what the handler actually sets.
    assert_eq!(body["data"]["transactions"].as_array().unwrap().len(), 1);

    // 5. Test Mutation: forceCompleteTransaction
    let mutation = json!({
        "query": format!("mutation {{ forceCompleteTransaction(id: \"{}\") {{ id status }} }}", tx_id)
    });
    let res = client.post(&graphql_url).json(&mutation).send().await.unwrap();
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["data"]["forceCompleteTransaction"]["status"], "completed");
}
