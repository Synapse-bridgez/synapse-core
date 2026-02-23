use synapse_core::{create_app, AppState};
use testcontainers_modules::postgres::Postgres;
use testcontainers::runners::AsyncRunner;
use sqlx::{PgPool, migrate::Migrator};
use std::path::Path;
use tokio::net::TcpListener;
use reqwest::StatusCode;
use serde_json::json;

async fn setup_test_app() -> (String, PgPool, impl std::any::Any) {
    let container = Postgres::default().start().await.unwrap();
    let host_port = container.get_host_port_ipv4(5432).await.unwrap();
    let database_url = format!("postgres://postgres:postgres@127.0.0.1:{}/postgres", host_port);

    let pool = PgPool::connect(&database_url).await.unwrap();
    let migrator = Migrator::new(Path::join(Path::new(env!("CARGO_MANIFEST_DIR")), "migrations")).await.unwrap();
    migrator.run(&pool).await.unwrap();

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

    let base_url = format!("http://{}", addr);
    (base_url, pool, container)
}

#[tokio::test]
async fn test_valid_deposit_flow() {
    let (base_url, _pool, _container) = setup_test_app().await;
    let client = reqwest::Client::new();

    let payload = json!({
        "stellar_account": "GABC1234567890",
        "amount": "100.50",
        "asset_code": "USD",
        "callback_type": "deposit",
        "callback_status": "completed"
    });

    let res = client.post(&format!("{}/callback", base_url))
        .header("X-App-Signature", "valid-signature")
        .json(&payload)
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::CREATED);
    let transaction: serde_json::Value = res.json().await.unwrap();
    let tx_id = transaction["id"].as_str().unwrap();

    let res = client.get(&format!("{}/transactions/{}", base_url, tx_id))
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

#[tokio::test]
async fn test_callback_with_memo_and_metadata() {
    let (base_url, _pool, _container) = setup_test_app().await;
    let client = reqwest::Client::new();

    let payload = json!({
        "stellar_account": "GDEF9876543210",
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

    let res = client.post(&format!("{}/callback", base_url))
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
    assert_eq!(transaction["metadata"]["customer_note"], "Monthly subscription");
    assert_eq!(transaction["metadata"]["compliance_tag"], "low_risk");

    let res = client.get(&format!("{}/transactions/{}", base_url, tx_id))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let fetched: serde_json::Value = res.json().await.unwrap();
    assert_eq!(fetched["memo"], "payment for invoice #1042");
    assert_eq!(fetched["memo_type"], "text");
    assert_eq!(fetched["metadata"]["reference_id"], "INV-1042");
}

#[tokio::test]
async fn test_callback_with_hash_memo_type() {
    let (base_url, _pool, _container) = setup_test_app().await;
    let client = reqwest::Client::new();

    let payload = json!({
        "stellar_account": "GHIJ5555555555",
        "amount": "500.00",
        "asset_code": "USD",
        "memo": "abc123def456",
        "memo_type": "hash"
    });

    let res = client.post(&format!("{}/callback", base_url))
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

#[tokio::test]
async fn test_callback_with_invalid_memo_type() {
    let (base_url, _pool, _container) = setup_test_app().await;
    let client = reqwest::Client::new();

    let payload = json!({
        "stellar_account": "GKLM7777777777",
        "amount": "100.00",
        "asset_code": "USD",
        "memo": "some memo",
        "memo_type": "invalid_type"
    });

    let res = client.post(&format!("{}/callback", base_url))
        .header("X-App-Signature", "valid-signature")
        .json(&payload)
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_callback_with_metadata_only() {
    let (base_url, _pool, _container) = setup_test_app().await;
    let client = reqwest::Client::new();

    let payload = json!({
        "stellar_account": "GNOP3333333333",
        "amount": "75.25",
        "asset_code": "EUR",
        "metadata": {
            "partner_ref": "P-9001",
            "tags": ["recurring", "verified"]
        }
    });

    let res = client.post(&format!("{}/callback", base_url))
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
async fn test_invalid_signature_flow() {
    let (base_url, _pool, _container) = setup_test_app().await;
    let client = reqwest::Client::new();

    let payload = json!({
        "stellar_account": "GABC1234567890",
        "amount": "100.50",
        "asset_code": "USD",
        "callback_type": "deposit",
        "callback_status": "completed"
    });

    let res = client.post(&format!("{}/callback", base_url))
        .header("X-App-Signature", "invalid-signature")
        .json(&payload)
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let error_res: serde_json::Value = res.json().await.unwrap();
    assert!(error_res["error"].as_str().unwrap().contains("Invalid signature"));
}
