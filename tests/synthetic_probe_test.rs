//! End-to-end synthetic probe checks: authenticated public routes must stay
//! outside tenant quotas and transaction-backed reporting aggregates.

mod common;

use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use sqlx::PgPool;
use std::time::{SystemTime, UNIX_EPOCH};
use common::TestApp;

const PROBE_SECRET: &str = "synthetic-probe-integration-test-secret";

fn sign_probe_body(body: &[u8]) -> (String, String) {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .to_string();
    let mut mac = Hmac::<Sha256>::new_from_slice(PROBE_SECRET.as_bytes()).unwrap();
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    (
        timestamp,
        format!("sha256={}", hex::encode(mac.finalize().into_bytes())),
    )
}

async fn count(pool: &PgPool, table: &str) -> i64 {
    let query = format!("SELECT COUNT(*) FROM {table}");
    sqlx::query_scalar(&query).fetch_one(pool).await.unwrap()
}

#[tokio::test]
#[ignore = "Requires Docker for the PostgreSQL testcontainer"]
async fn synthetic_callback_and_graphql_do_not_write_customer_data() {
    std::env::set_var("SYNTHETIC_PROBE_SECRET", PROBE_SECRET);
    let app = TestApp::new().await;
    let client = reqwest::Client::new();
    let before_transactions = count(&app.pool, "transactions").await;
    let before_daily_report = count(&app.pool, "mv_daily_transaction_volume").await;
    let before_hourly_report = count(&app.pool, "mv_hourly_request_volume").await;

    let callback_body = serde_json::to_vec(&serde_json::json!({
        "stellar_account": format!("G{}", "A".repeat(55)),
        "amount": "1.00",
        "asset_code": "USD",
        "callback_type": "deposit",
        "callback_status": "pending",
        "anchor_transaction_id": "synthetic-probe"
    }))
    .unwrap();
    let (timestamp, signature) = sign_probe_body(&callback_body);
    let response = client
        .post(format!("{}/__synthetic/callback", app.base_url))
        .header("content-type", "application/json")
        .header("X-Synthetic-Probe-Timestamp", timestamp)
        .header("X-Synthetic-Probe-Signature", signature)
        .body(callback_body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CREATED);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["synthetic"], true);

    let graphql_body = serde_json::to_vec(&serde_json::json!({
        "query": "query SyntheticProbe { __typename }"
    }))
    .unwrap();
    let (timestamp, signature) = sign_probe_body(&graphql_body);
    let response = client
        .post(format!("{}/__synthetic/graphql", app.base_url))
        .header("content-type", "application/json")
        .header("X-Synthetic-Probe-Timestamp", timestamp)
        .header("X-Synthetic-Probe-Signature", signature)
        .body(graphql_body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["data"]["__typename"], "Query");
    assert!(body.get("errors").is_none());

    assert_eq!(count(&app.pool, "transactions").await, before_transactions);
    assert_eq!(
        count(&app.pool, "mv_daily_transaction_volume").await,
        before_daily_report
    );
    assert_eq!(
        count(&app.pool, "mv_hourly_request_volume").await,
        before_hourly_report
    );
}
