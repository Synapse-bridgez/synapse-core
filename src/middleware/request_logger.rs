use axum::{body::Body, http::Request, middleware::Next, response::Response};
use std::time::Instant;
use uuid::Uuid;

pub async fn request_logger_middleware(mut req: Request<Body>, next: Next<Body>) -> Response {
    let request_id = Uuid::new_v4().to_string();
    let method = req.method().clone();
    let uri = req.uri().clone();
    let start = Instant::now();

    // Insert request ID into headers for downstream handlers
    req.headers_mut()
        .insert("x-request-id", request_id.parse().unwrap());

    // Log request
    tracing::info!(
        request_id = %request_id,
        method = %method,
        uri = %uri,
        "Incoming request"
    );

    // Process request
    let response: Response = next.run(req).await;

    let latency = start.elapsed();
    let status = response.status();

    // Log response
    tracing::info!(
        request_id = %request_id,
        method = %method,
        uri = %uri,
        status = %status.as_u16(),
        latency_ms = %latency.as_millis(),
        "Request completed"
    );

    // Add request ID to response headers
    let (mut parts, body) = response.into_parts();
    parts
        .headers
        .insert("x-request-id", request_id.parse().unwrap());

    Response::from_parts(parts, body)
}
