use axum::{
    body::Body,
    http::{Request, StatusCode},
    middleware,
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use serde_json::json;
use tower::ServiceExt;
use tracing_test::traced_test;

// Helper function to create a test app with request logger middleware
fn create_test_app() -> Router {
    async fn test_handler() -> impl IntoResponse {
        (StatusCode::OK, "success")
    }

    async fn test_handler_with_query() -> impl IntoResponse {
        (StatusCode::OK, "query handled")
    }

    async fn test_handler_error() -> impl IntoResponse {
        (StatusCode::INTERNAL_SERVER_ERROR, "error occurred")
    }

    Router::new()
        .route("/test", post(test_handler))
        .route("/query", get(test_handler_with_query))
        .route("/error", get(test_handler_error))
        .layer(middleware::from_fn(
            synapse_core::middleware::request_logger::request_logger_middleware,
        ))
}

#[tokio::test]
async fn test_request_id_generation() {
    let app = create_test_app();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // Verify request ID is present in response headers
    assert!(response.headers().contains_key("x-request-id"));

    let request_id = response.headers().get("x-request-id").unwrap();
    let request_id_str = request_id.to_str().unwrap();

    // Verify it's a valid UUID format
    assert_eq!(request_id_str.len(), 36); // UUID v4 format: xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx
    assert_eq!(request_id_str.chars().filter(|&c| c == '-').count(), 4);
}

#[tokio::test]
async fn test_request_id_uniqueness() {
    let app1 = create_test_app();
    let app2 = create_test_app();

    let response1 = app1
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let response2 = app2
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let request_id1 = response1
        .headers()
        .get("x-request-id")
        .unwrap()
        .to_str()
        .unwrap();
    let request_id2 = response2
        .headers()
        .get("x-request-id")
        .unwrap()
        .to_str()
        .unwrap();

    // Verify each request gets a unique ID
    assert_ne!(request_id1, request_id2);
}

#[tokio::test]
async fn test_request_logging_methods() {
    let app = create_test_app();

    // Test POST method
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key("x-request-id"));

    // Test GET method
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/query")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key("x-request-id"));
}

#[tokio::test]
async fn test_request_logging_query_params() {
    let app = create_test_app();

    // Test with query parameters
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/query?page=1&limit=10&filter=active")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key("x-request-id"));
}

#[tokio::test]
async fn test_request_logging_errors() {
    let app = create_test_app();

    // Test error response logging
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/error")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // Verify error status is returned
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

    // Verify request ID is still present even on error
    assert!(response.headers().contains_key("x-request-id"));
}

#[tokio::test]
async fn test_request_logging_with_body() {
    // Set environment variable to enable body logging
    std::env::set_var("LOG_REQUEST_BODY", "true");

    let app = create_test_app();

    let payload = json!({
        "user": "john_doe",
        "amount": "100.50",
        "asset_code": "USD"
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/test")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key("x-request-id"));

    // Clean up
    std::env::remove_var("LOG_REQUEST_BODY");
}

#[tokio::test]
#[traced_test]
async fn test_request_logging_sanitization() {
    // Enable body logging so the middleware logs (and sanitizes) the request body.
    std::env::set_var("LOG_REQUEST_BODY", "true");

    let app = create_test_app();

    // Payload with sensitive data whose plain-text values must never appear in logs.
    let payload = json!({
        "stellar_account": "GABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890",
        "password": "super_secret_password",
        "token": "secret_token_12345",
        "amount": "100.50",
        "asset_code": "USD"
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/test")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key("x-request-id"));

    // The log body must contain the masking marker for every sensitive field.
    assert!(
        logs_contain("****"),
        "Expected at least one masked field in logs"
    );

    // Sensitive field values must not appear in plain text.
    assert!(
        !logs_contain("super_secret_password"),
        "Plain-text password must not appear in logs"
    );
    assert!(
        !logs_contain("secret_token_12345"),
        "Plain-text token must not appear in logs"
    );
    // stellar_account value is long enough to be partially shown, but the full
    // 36-char account string should never be logged verbatim.
    assert!(
        !logs_contain("GABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890"),
        "Full stellar_account value must not appear in logs"
    );

    // Non-sensitive fields must still be logged clearly.
    assert!(
        logs_contain("100.50"),
        "Non-sensitive 'amount' field should be visible in logs"
    );

    // Clean up
    std::env::remove_var("LOG_REQUEST_BODY");
}

#[tokio::test]
#[traced_test]
async fn test_request_logging_nested_sensitive_data() {
    // Enable body logging so the middleware logs (and sanitizes) the request body.
    std::env::set_var("LOG_REQUEST_BODY", "true");

    let app = create_test_app();

    // Payload with sensitive data buried inside nested objects.
    let payload = json!({
        "transaction": {
            "stellar_account": "GABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890",
            "amount": "100.50"
        },
        "user": {
            "name": "John Doe",
            "api_key": "secret_api_key_12345"
        }
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/test")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key("x-request-id"));

    // The log body must contain the masking marker, proving sanitization ran.
    assert!(
        logs_contain("****"),
        "Expected at least one masked field in logs"
    );

    // Nested sensitive values must not appear in plain text.
    assert!(
        !logs_contain("secret_api_key_12345"),
        "Plain-text api_key must not appear in logs"
    );
    assert!(
        !logs_contain("GABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890"),
        "Full stellar_account value must not appear in logs"
    );

    // Non-sensitive fields at any depth must still be logged.
    assert!(
        logs_contain("100.50"),
        "Non-sensitive 'amount' field should be visible in logs"
    );
    assert!(
        logs_contain("John Doe"),
        "Non-sensitive 'name' field should be visible in logs"
    );

    // Clean up
    std::env::remove_var("LOG_REQUEST_BODY");
}

#[tokio::test]
async fn test_request_logging_large_body() {
    // Set environment variable to enable body logging
    std::env::set_var("LOG_REQUEST_BODY", "true");

    let app = create_test_app();

    // Create a large payload (larger than MAX_BODY_LOG_SIZE which is 1KB)
    let large_string = "x".repeat(2000); // 2KB
    let payload = json!({
        "data": large_string
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/test")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    // Large payloads are accepted; logging is truncated
    assert_eq!(response.status(), StatusCode::OK);

    // Clean up
    std::env::remove_var("LOG_REQUEST_BODY");
}

#[tokio::test]
async fn test_request_logging_non_json_body() {
    // Set environment variable to enable body logging
    std::env::set_var("LOG_REQUEST_BODY", "true");

    let app = create_test_app();

    // Send non-JSON body
    let body = "This is plain text, not JSON";

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/test")
                .header("content-type", "text/plain")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key("x-request-id"));

    // Clean up
    std::env::remove_var("LOG_REQUEST_BODY");
}

#[tokio::test]
async fn test_request_logging_without_body_logging() {
    // Ensure body logging is disabled
    std::env::remove_var("LOG_REQUEST_BODY");

    let app = create_test_app();

    let payload = json!({
        "user": "john_doe",
        "amount": "100.50"
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/test")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key("x-request-id"));
}

#[tokio::test]
async fn test_request_logging_empty_body() {
    // Set environment variable to enable body logging
    std::env::set_var("LOG_REQUEST_BODY", "true");

    let app = create_test_app();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key("x-request-id"));

    // Clean up
    std::env::remove_var("LOG_REQUEST_BODY");
}

#[tokio::test]
async fn test_request_logging_multiple_requests() {
    let app = create_test_app();

    // Send multiple requests and verify each gets unique request ID
    let mut request_ids = Vec::new();

    for _ in 0..5 {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let request_id = response
            .headers()
            .get("x-request-id")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        request_ids.push(request_id);
    }

    // Verify all request IDs are unique
    let unique_count = request_ids
        .iter()
        .collect::<std::collections::HashSet<_>>()
        .len();
    assert_eq!(unique_count, 5);
}
