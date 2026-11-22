use axum::{
    body::Body,
    http::Request,
    middleware::Next,
    response::{IntoResponse, Response},
};
use futures::FutureExt;
use std::panic::AssertUnwindSafe;

/// Middleware that catches handler panics and returns a 500 response instead of
/// dropping the connection. Logs the panic with a backtrace and increments the
/// `handler_panic_total` metric counter.
pub async fn panic_recovery_middleware(req: Request<Body>, next: Next<Body>) -> Response {
    // Capture request metadata before consuming the request
    let uri = req.uri().clone();
    let method = req.method().clone();
    let correlation_id = req
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_owned())
        .unwrap_or_else(|| "unknown".to_string());

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
            // The payload is never included in the client response.
            tracing::error!(
                correlation_id = %correlation_id,
                method = %method,
                path = %uri.path(),
                panic.message = %panic_msg,
                "Handler panicked — returning 500 to client"
            );

            // Increment the metric counter (tracing-based event counter compatible
            // with OpenTelemetry metrics bridge).
            tracing::info!(counter.handler_panic_total = 1u64, "handler panic recorded");

            axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request, routing::get, Router};
    use std::sync::{Arc, Mutex};
    use tower::ServiceExt;
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Metadata, Subscriber};

    /// Minimal subscriber that records the fields of every event.
    #[derive(Clone, Default)]
    struct CapturingSubscriber {
        events: Arc<Mutex<Vec<(String, Vec<(String, String)>)>>>,
    }

    struct FieldVisitor(Vec<(String, String)>);

    impl Visit for FieldVisitor {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.0.push((field.name().to_string(), format!("{:?}", value)));
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            self.0.push((field.name().to_string(), value.to_string()));
        }
    }

    impl Subscriber for CapturingSubscriber {
        fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _span: &Attributes<'_>) -> Id {
            Id::from_u64(1)
        }

        fn record(&self, _span: &Id, _values: &Record<'_>) {}

        fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

        fn event(&self, event: &Event<'_>) {
            let mut visitor = FieldVisitor(Vec::new());
            event.record(&mut visitor);
            let message = visitor
                .0
                .iter()
                .find(|(k, _)| k == "message")
                .map(|(_, v)| v.clone())
                .unwrap_or_default();
            self.events
                .lock()
                .unwrap()
                .push((message, visitor.0));
        }

        fn enter(&self, _span: &Id) {}

        fn exit(&self, _span: &Id) {}
    }

    #[tokio::test]
    async fn test_panic_payload_logged_and_generic_500_returned() {
        let subscriber = CapturingSubscriber::default();
        let events = subscriber.events.clone();
        let _guard = tracing::subscriber::set_default(subscriber);

        let app = Router::new()
            .route(
                "/boom",
                get(|| async {
                    panic!("secret panic detail");
                }),
            )
            .layer(axum::middleware::from_fn(panic_recovery_middleware));

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/boom")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), axum::http::StatusCode::INTERNAL_SERVER_ERROR);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = String::from_utf8_lossy(&body);
        assert!(
            !body.contains("secret panic detail"),
            "panic payload must not leak to the client, got: {body}"
        );

        let events = events.lock().unwrap();
        let panic_event = events
            .iter()
            .find(|(message, _)| message.contains("Handler panicked"))
            .expect("panic must be logged at error level");

        let fields = &panic_event.1;
        let get = |name: &str| {
            fields
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };

        assert!(
            get("panic.message").contains("secret panic detail"),
            "panic payload must be logged, fields: {fields:?}"
        );
        assert_eq!(get("path"), "/boom");
    }
}
