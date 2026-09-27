//! Demonstrates using the `testing` module as an external-style consumer of
//! this crate would: via `synapse_sdk::testing`, gated on the
//! `testing-support` feature. Run with:
//!
//!     cargo test -p synapse-sdk --features testing-support --test testing_support_example

#![cfg(feature = "testing-support")]

use synapse_sdk::error::CatalogEntry;
use synapse_sdk::testing::{spawn_mock_server, stub_endpoint, stub_error, stub_rate_limited};

#[tokio::test]
async fn stubs_a_success_response() {
    let server = spawn_mock_server().await;
    stub_endpoint(
        &server,
        "GET",
        "/v1/transactions/123",
        200,
        serde_json::json!({"id": "123", "status": "settled"}),
    )
    .await;

    let resp = reqwest::get(format!("{}/v1/transactions/123", server.uri()))
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "settled");
}

#[tokio::test]
async fn stubs_a_catalog_error_response() {
    let server = spawn_mock_server().await;
    let entry = CatalogEntry {
        code: "invalid_cursor".into(),
        http_status: 400,
        description: "the pagination cursor was invalid or expired".into(),
    };
    stub_error(&server, "GET", "/v1/transactions", &entry).await;

    let resp = reqwest::get(format!("{}/v1/transactions", server.uri()))
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["code"], "invalid_cursor");
}

#[tokio::test]
async fn stubs_a_rate_limited_response_with_retry_after() {
    let server = spawn_mock_server().await;
    stub_rate_limited(&server, "POST", "/v1/webhooks/replay", 30, "slow down").await;

    let resp = reqwest::Client::new()
        .post(format!("{}/v1/webhooks/replay", server.uri()))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 429);
    assert_eq!(
        resp.headers().get("retry-after").unwrap().to_str().unwrap(),
        "30"
    );
}
