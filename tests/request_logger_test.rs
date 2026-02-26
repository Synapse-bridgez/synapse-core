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

    // Verify the request was processed successfully with query params
    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_str = String::from_utf8(body_bytes.to_vec()).unwrap();
    assert_eq!(body_str, "query handled");
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

    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body_str = String::from_utf8(body_bytes.to_vec()).unwrap();
    assert_eq!(body_str, "error occurred");
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
async fn test_request_logging_sanitization() {
    // Set environment variable to enable body logging
    std::env::set_var("LOG_REQUEST_BODY", "true");

    let app = create_test_app();

    // Payload with sensitive data
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

    // Note: The actual sanitization happens in the logs, which we can't directly test here
    // But we verify the request is processed successfully with sensitive data
    // The sanitize_json function is tested separately in src/utils/sanitize.rs

    // Clean up
    std::env::remove_var("LOG_REQUEST_BODY");
}

#[tokio::test]
async fn test_request_logging_nested_sensitive_data() {
    // Set environment variable to enable body logging
    std::env::set_var("LOG_REQUEST_BODY", "true");

    let app = create_test_app();

    // Payload with nested sensitive data
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

    // Should return PAYLOAD_TOO_LARGE status
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

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

    for i in 0..5 {
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
