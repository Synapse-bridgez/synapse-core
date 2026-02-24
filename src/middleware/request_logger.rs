use axum::{
    body::Body,
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::time::Instant;
use uuid::Uuid;

const MAX_BODY_LOG_SIZE: usize = 1024; // 1KB limit for body logging

pub async fn request_logger_middleware(mut req: Request, next: Next) -> Response {
    let request_id = Uuid::new_v4().to_string();
    let method = req.method().clone();
    let uri = req.uri().clone();
    let start = Instant::now();

    // Insert request ID into headers for downstream handlers
    req.headers_mut().insert(
        "x-request-id",
        request_id.parse().unwrap(),
    );

    // Log request
    let log_body = std::env::var("LOG_REQUEST_BODY")
        .unwrap_or_else(|_| "false".to_string())
        .parse::<bool>()
        .unwrap_or(false);

    if log_body {
        // Extract and log body if enabled (with size limit)
        let (parts, body) = req.into_parts();
        let bytes = match axum::body::to_bytes(body, MAX_BODY_LOG_SIZE).await {
            Ok(bytes) => bytes,
            Err(_) => {
                tracing::warn!(
                    request_id = %request_id,
                    method = %method,
                    uri = %uri,
                    "Request body too large or failed to read"
                );
                return (StatusCode::PAYLOAD_TOO_LARGE, "Request body too large").into_response();
            }
        };

        let body_str = String::from_utf8_lossy(&bytes);
        let sanitized_body = if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body_str) {
            let sanitized = crate::utils::sanitize::sanitize_json(&json);
            serde_json::to_string(&sanitized).unwrap_or_else(|_| "[invalid json]".to_string())
        } else {
            format!("[non-json, {} bytes]", bytes.len())
        };

        tracing::info!(
            request_id = %request_id,
            method = %method,
            uri = %uri,
            body_size = bytes.len(),
            body = %sanitized_body,
            "Incoming request"
        );

        // Reconstruct request with body
        req = Request::from_parts(parts, Body::from(bytes));
    } else {
        tracing::info!(
            request_id = %request_id,
            method = %method,
            uri = %uri,
            "Incoming request"
        );
    }

    // Process request
    let response = next.run(req).await;
    
    let latency = start.elapsed();
    let status = response.status();

    // Log response
    tracing::info!(
        request_id = %request_id,
        method = %method,
        uri = %uri,
        status = %status.as_u16(),
        latency_ms = latency.as_millis(),
        "Outgoing response"
    );

    // Add request ID to response headers
    let (mut parts, body) = response.into_parts();
    parts.headers.insert(
        "x-request-id",
        request_id.parse().unwrap(),
    );

    Response::from_parts(parts, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, routing::post, Router};
    use axum::http::Request;
    use tower::ServiceExt;

    #[tokio::test]
    async fn test_request_logger_adds_request_id() {
        let app = Router::new()
            .route("/test", post(|| async { "ok" }))
            .layer(axum::middleware::from_fn(request_logger_middleware));

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

        assert!(response.headers().contains_key("x-request-id"));
    }
}
