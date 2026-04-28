use axum::{
    body::Body,
    http::Request,
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::panic::AssertUnwindSafe;
use futures::FutureExt;

/// Middleware that catches handler panics and returns a 500 response instead of
/// dropping the connection. Logs the panic with a backtrace and increments the
/// `handler_panic_total` metric counter.
pub async fn panic_recovery_middleware(req: Request<Body>, next: Next<Body>) -> Response {
    // Capture the URI before consuming the request
    let uri = req.uri().to_string();
    let method = req.method().to_string();

    let result = AssertUnwindSafe(next.run(req)).catch_unwind().await;

    match result {
        Ok(response) => response,
        Err(panic_payload) => {
            // Extract a human-readable panic message
            let panic_msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                s.to_string()
            } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "unknown panic payload".to_string()
            };

            // Log at error level with as much context as possible.
            // RUST_BACKTRACE=1 must be set for the backtrace to appear in logs.
            tracing::error!(
                panic.message = %panic_msg,
                http.method = %method,
                http.target = %uri,
                "Handler panicked — returning 500 to client"
            );

            // Increment the metric counter (tracing-based event counter compatible
            // with OpenTelemetry metrics bridge).
            tracing::info!(counter.handler_panic_total = 1u64, "handler panic recorded");

            axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
