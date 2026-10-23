use axum::{http::StatusCode, response::IntoResponse};
use synapse_core::error::SwapApiError;

#[tokio::test]
async fn every_swap_variant_has_a_stable_problem_contract() {
    use SwapApiError::*;
    let cases = [
        (Validation, "SWAP_VALIDATION", 400),
        (UnsupportedAsset, "SWAP_UNSUPPORTED_ASSET", 422),
        (StaleQuote, "SWAP_STALE_QUOTE", 409),
        (LimitExceeded, "SWAP_LIMIT_EXCEEDED", 429),
        (Expired, "SWAP_EXPIRED", 410),
        (UpstreamFailure, "SWAP_UPSTREAM_FAILURE", 502),
        (SettlementFailure, "SWAP_SETTLEMENT_FAILURE", 502),
        (Internal, "SWAP_INTERNAL", 500),
        (Unauthorized, "SWAP_UNAUTHORIZED", 401),
        (Forbidden, "SWAP_FORBIDDEN", 403),
        (NotFound, "SWAP_NOT_FOUND", 404),
        (MethodNotAllowed, "SWAP_METHOD_NOT_ALLOWED", 405),
        (Unavailable, "SWAP_UNAVAILABLE", 503),
    ];
    for (variant, code, status) in cases {
        let response = variant.into_response();
        assert_eq!(response.status().as_u16(), status);
        assert_eq!(response.headers()["content-type"], "application/problem+json");
        assert_eq!(response.headers()["cache-control"], "no-store");
        if status == 503 || status == 429 {
            assert_eq!(response.headers()["retry-after"], "1");
        }
        let body = hyper::body::to_bytes(response.into_body()).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["code"], code);
        assert_eq!(value["status"], status);
        assert_eq!(value["type"], format!("urn:synapse:swap:problem:{code}"));
        assert!(!value["title"].as_str().unwrap().is_empty());
        assert!(!value["detail"].as_str().unwrap().is_empty());
        assert_eq!(value.as_object().unwrap().len(), 5);
    }
}

#[tokio::test]
async fn internal_causes_are_not_exposed() {
    let error = SwapApiError::from(anyhow::anyhow!("postgres://secret:password@internal/orders"));
    let response = error.into_response();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let bytes = hyper::body::to_bytes(response.into_body()).await.unwrap();
    let body = String::from_utf8(bytes.to_vec()).unwrap();
    assert!(body.contains("SWAP_INTERNAL"));
    assert!(!body.contains("password"));
    assert_eq!(SwapApiError::from(sqlx::Error::RowNotFound), SwapApiError::Internal);
}

#[tokio::test]
async fn unknown_infrastructure_responses_and_extractor_errors_are_normalized() {
    use axum::{middleware, routing::get, Router};
    use tower::ServiceExt;
    let app = Router::new()
        .route("/failure", get(|| async { (StatusCode::INTERNAL_SERVER_ERROR, "sensitive SQL") }))
        .route("/extract", get(|_: axum::extract::Query<std::collections::HashMap<String, u64>>| async { "ok" }))
        .layer(middleware::from_fn(synapse_core::error::swap_problem_middleware));
    for (path, expected) in [("/failure", "SWAP_INTERNAL"), ("/extract?limit=invalid", "SWAP_VALIDATION")] {
        let response = app.clone().oneshot(axum::http::Request::builder().uri(path).body(axum::body::Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.headers()["content-type"], "application/problem+json");
        let bytes = hyper::body::to_bytes(response.into_body()).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["code"], expected);
        assert!(!String::from_utf8_lossy(&bytes).contains("sensitive SQL"));
    }
}
