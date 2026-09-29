use axum::{
    body::Body,
    http::{HeaderMap, Request},
    middleware::Next,
    response::Response,
};
use opentelemetry::{
    propagation::Extractor,
    trace::TraceContextExt,
    Context,
};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use tracing::Instrument;
use tracing_opentelemetry::OpenTelemetrySpanExt;

struct HeaderMapExtractor<'a>(&'a HeaderMap);

impl Extractor for HeaderMapExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|value| value.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|key| key.as_str()).collect()
    }
}

fn extract_trace_context(headers: &HeaderMap) -> Context {
    TraceContextPropagator::new().extract(&HeaderMapExtractor(headers))
}

pub async fn trace_context_middleware(req: Request<Body>, next: Next<Body>) -> Response {
    let parent_context = extract_trace_context(req.headers());
    let span = tracing::info_span!(
        "http.request",
        otel.kind = "server",
        http.request.method = %req.method(),
    );
    span.set_parent(parent_context);

    let response = next.run(req).instrument(span.clone()).await;
    if response.status().is_server_error() {
        tracing::error!(
            parent: &span,
            http.response.status_code = response.status().as_u16(),
            "HTTP request failed"
        );
    }

    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_remote_w3c_trace_context() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "traceparent",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
                .parse()
                .unwrap(),
        );

        let context = extract_trace_context(&headers);
        let span_context = context.span().span_context();

        assert!(span_context.is_valid());
        assert!(span_context.is_remote());
        assert_eq!(
            span_context.trace_id().to_string(),
            "4bf92f3577b34da6a3ce929d0e0e4736"
        );
    }
}
