//! Regression test for issue #1060 Part F: `error_enrichment_middleware` existed
//! but was never mounted on the router, so error responses never got a real
//! `request_id` field injected into their JSON bodies.
//!
//! This asserts the specific invariant the issue calls out directly: an error
//! response's JSON body `request_id` must match its `X-Request-Id` response
//! header, through the real `create_app` middleware stack (not a hand-built
//! router), against a real running server.

mod common;

use reqwest::StatusCode;
use uuid::Uuid;

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_error_response_body_request_id_matches_header() {
    let app = common::TestApp::new().await;
    let client = reqwest::Client::new();

    // A random, never-inserted transaction id — guaranteed to 404.
    let missing_id = Uuid::new_v4();
    let res = client
        .get(format!("{}/transactions/{}", app.base_url, missing_id))
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    let header_request_id = res
        .headers()
        .get("x-request-id")
        .expect("response must carry an X-Request-Id header")
        .to_str()
        .unwrap()
        .to_string();

    let body: serde_json::Value = res.json().await.unwrap();
    let body_request_id = body
        .get("request_id")
        .and_then(|v| v.as_str())
        .expect("error response body must contain a request_id field");

    assert_ne!(
        body_request_id, "unknown",
        "request_id must be the real correlation id, not the enrichment \
         middleware's fallback — this is exactly what wrong middleware \
         ordering produces"
    );
    assert_eq!(
        body_request_id, header_request_id,
        "error body request_id must match the X-Request-Id response header"
    );
}
