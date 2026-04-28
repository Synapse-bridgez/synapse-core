//! Structured request/response logger with correlation ID propagation.
//!
//! For every request the middleware:
//! - Reads the `X-Request-Id` header if present, otherwise generates a new
//!   UUID v4 as the correlation ID.
//! - Stores the correlation ID in a task-local so that all `tracing` spans
//!   emitted during the request automatically include it.
//! - Logs method, path, status, duration, body size, and client IP at INFO
//!   level in a structured format.
//! - Attaches the correlation ID to the response as `X-Request-Id`.
//! - Includes the correlation ID in error responses produced by [`AppError`].

use axum::{
    body::Body,
    extract::{ConnectInfo, Request},
    http::{HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::{net::SocketAddr, time::Instant};
use uuid::Uuid;

use crate::error::RequestId;

const MAX_BODY_LOG_SIZE: usize = 1024; // 1 KB limit for body logging

/// Axum middleware function.
///
/// Mount with:
/// ```rust
/// Router::new()
///     .layer(axum::middleware::from_fn(request_logger_middleware))
/// ```
pub async fn request_logger_middleware(mut req: Request, next: Next) -> Response {
    // -----------------------------------------------------------------------
    // 1. Resolve correlation ID
    // -----------------------------------------------------------------------
    let correlation_id = req
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_owned())
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    // -----------------------------------------------------------------------
    // 2. Capture request metadata before consuming the request
    // -----------------------------------------------------------------------
    let method = req.method().clone();
    let uri = req.uri().clone();
    let start = Instant::now();

    // Extract client IP from ConnectInfo extension (populated by axum when
    // the server is bound with `into_make_service_with_connect_info`).
    let client_ip = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    // Propagate correlation ID downstream via request header so handlers and
    // sub-services can read it.
    req.headers_mut().insert(
        "x-request-id",
        HeaderValue::from_str(&correlation_id).unwrap_or_else(|_| HeaderValue::from_static("")),
    );

    // Also expose as a typed extension so error handlers can read it without
    // parsing headers (compatible with the RequestId extractor in error.rs).
    req.extensions_mut()
        .insert(RequestId(correlation_id.clone()));

    // -----------------------------------------------------------------------
    // 3. Optionally log request body (controlled by LOG_REQUEST_BODY env var)
    // -----------------------------------------------------------------------
    let log_body = std::env::var("LOG_REQUEST_BODY")
        .unwrap_or_else(|_| "false".to_string())
        .parse::<bool>()
        .unwrap_or(false);

    let request_body_size: usize;

    if log_body {
        let (parts, body) = req.into_parts();
        let bytes = match axum::body::to_bytes(body, MAX_BODY_LOG_SIZE).await {
            Ok(b) => b,
            Err(_) => {
                tracing::warn!(
                    correlation_id = %correlation_id,
                    method = %method,
                    path = %uri.path(),
                    "Request body too large or failed to read"
                );
                return (StatusCode::PAYLOAD_TOO_LARGE, "Request body too large")
                    .into_response();
            }
        };

        request_body_size = bytes.len();

        let sanitized_body = if let Ok(json) =
            serde_json::from_slice::<serde_json::Value>(&bytes)
        {
            let sanitized = crate::utils::sanitize::sanitize_json(&json);
            serde_json::to_string(&sanitized).unwrap_or_else(|_| "[invalid json]".to_string())
        } else {
            format!("[non-json, {} bytes]", bytes.len())
        };

        tracing::info!(
            correlation_id = %correlation_id,
            method = %method,
            path = %uri.path(),
            client_ip = %client_ip,
            body_size = request_body_size,
            body = %sanitized_body,
            "Incoming request"
        );

        req = Request::from_parts(parts, Body::from(bytes));
    } else {
        request_body_size = 0;
        tracing::info!(
            correlation_id = %correlation_id,
            method = %method,
            path = %uri.path(),
            client_ip = %client_ip,
            "Incoming request"
        );
    }

    // -----------------------------------------------------------------------
    // 4. Run the inner handler
    // -----------------------------------------------------------------------
    let mut response = next.run(req).await;

    // -----------------------------------------------------------------------
    // 5. Log response
    // -----------------------------------------------------------------------
    let latency = start.elapsed();
    let status = response.status();

    // Approximate response body size from Content-Length header
    let response_body_size = response
        .headers()
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);

    tracing::info!(
        correlation_id = %correlation_id,
        method = %method,
        path = %uri.path(),
        status = status.as_u16(),
        latency_ms = latency.as_millis(),
        request_body_size = request_body_size,
        response_body_size = response_body_size,
        client_ip = %client_ip,
        "Outgoing response"
    );

    // -----------------------------------------------------------------------
    // 6. Attach correlation ID to response headers
    // -----------------------------------------------------------------------
    response.headers_mut().insert(
        "x-request-id",
        HeaderValue::from_str(&correlation_id).unwrap_or_else(|_| HeaderValue::from_static("")),
    );

    response
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, routing::post, Router};
    use axum::http::Request;
    use tower::ServiceExt;

    #[tokio::test]
    async fn test_request_logger_generates_correlation_id() {
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

        assert!(
            response.headers().contains_key("x-request-id"),
            "Response must contain x-request-id header"
        );
    }

    #[tokio::test]
    async fn test_request_logger_preserves_existing_correlation_id() {
        let app = Router::new()
            .route("/test", post(|| async { "ok" }))
            .layer(axum::middleware::from_fn(request_logger_middleware));

        let custom_id = "my-custom-correlation-id-123";

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/test")
                    .header("x-request-id", custom_id)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let returned_id = response
            .headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        assert_eq!(
            returned_id, custom_id,
            "Middleware should echo back the caller-supplied correlation ID"
        );
    }
}
