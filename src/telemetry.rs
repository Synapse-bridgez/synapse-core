//! OpenTelemetry initialisation.
//!
//! Call `init_tracer` once at startup.  It returns a `TracerProvider` that
//! must be kept alive for the duration of the process (dropping it flushes
//! and shuts down the exporter).  When no OTLP endpoint is configured the
//! function installs a no-op provider so the rest of the code compiles and
//! runs unchanged.

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{
    propagation::TraceContextPropagator,
    runtime,
    trace::{self as sdktrace, TracerProvider},
    Resource,
};
use opentelemetry_semantic_conventions::resource::{SERVICE_NAME, SERVICE_VERSION};

/// Initialise the global tracer and return the provider so the caller can
/// shut it down cleanly on exit.
pub fn init_tracer(
    service_name: &str,
    otlp_endpoint: Option<&str>,
) -> anyhow::Result<TracerProvider> {
    // W3C TraceContext propagation (traceparent / tracestate headers)
    opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());

    let resource = Resource::new(vec![
        opentelemetry::KeyValue::new(SERVICE_NAME, service_name.to_string()),
        opentelemetry::KeyValue::new(SERVICE_VERSION, env!("CARGO_PKG_VERSION")),
    ]);

    let provider = match otlp_endpoint {
        Some(endpoint) => {
            let exporter = opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint(endpoint)
                .build_span_exporter()?;

            let provider = sdktrace::TracerProvider::builder()
                .with_config(sdktrace::Config::default().with_resource(resource))
                .with_batch_exporter(exporter, runtime::Tokio)
                .build();

            tracing::info!("OpenTelemetry OTLP exporter configured → {endpoint}");
            provider
        }
        None => {
            let provider = sdktrace::TracerProvider::builder()
                .with_config(sdktrace::Config::default().with_resource(resource))
                .build();

            tracing::info!("No OTLP_ENDPOINT set — OpenTelemetry running in no-op mode");
            provider
        }
    };

    // Register as the global provider so `opentelemetry::global::tracer()`
    // works anywhere in the codebase.
    opentelemetry::global::set_tracer_provider(provider.clone());

    Ok(provider)
}

/// Shut down the tracer provider, flushing any buffered spans.
pub fn shutdown_tracer(provider: TracerProvider) {
    let results = provider.force_flush();
    for r in results {
        if let Err(e) = r {
            tracing::error!("OpenTelemetry flush error: {e}");
        }
    }
    drop(provider);
}
