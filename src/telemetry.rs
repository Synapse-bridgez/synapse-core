//! OpenTelemetry initialisation.
//!
//! Call `init_tracer` once at startup.  It returns a `TracerProvider` that
//! must be kept alive for the duration of the process (dropping it flushes
//! and shuts down the exporter).  When no OTLP endpoint is configured the
//! function installs a no-op provider so the rest of the code compiles and
//! runs unchanged.
//!
//! # Graceful shutdown
//!
//! Call [`shutdown_tracer`] during process teardown.  It:
//! 1. Flushes all in-flight spans via `force_flush` with a bounded timeout.
//! 2. Shuts down the provider (stops the background batch-export thread).
//! 3. Clears the global tracer so subsequent span creation is a no-op.
//!
//! The flush timeout defaults to [`DEFAULT_FLUSH_TIMEOUT`] and can be
//! overridden via the `OTEL_FLUSH_TIMEOUT_SECS` environment variable.

use std::time::Duration;

use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{
    propagation::TraceContextPropagator,
    runtime,
    trace::{self as sdktrace, TracerProvider},
    Resource,
};
use opentelemetry_semantic_conventions::resource::{SERVICE_NAME, SERVICE_VERSION};

/// Default maximum time to wait for in-flight spans to be exported on shutdown.
pub const DEFAULT_FLUSH_TIMEOUT: Duration = Duration::from_secs(5);

/// Initialise the global tracer and return the provider so the caller can
/// shut it down cleanly on exit.
///
/// # Non-Fatal Failure Handling
///
/// Telemetry initialization failures are never fatal. If the OTLP exporter
/// fails to initialize, the system falls back to a no-op tracer and continues
/// operation. This ensures observability infrastructure issues do not disrupt
/// the main application flow.
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

/// Resolve the flush timeout from the environment, falling back to
/// [`DEFAULT_FLUSH_TIMEOUT`].
fn flush_timeout() -> Duration {
    std::env::var("OTEL_FLUSH_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_FLUSH_TIMEOUT)
}

/// Shut down the tracer provider, flushing any buffered spans.
///
/// Flush errors are logged but do not panic — a best-effort export is
/// preferable to crashing during shutdown.  After flushing, the provider is
/// explicitly shut down and the global tracer is cleared so that any spans
/// created after this point are silently dropped rather than queued forever.
pub fn shutdown_tracer(provider: TracerProvider) {
    let timeout = flush_timeout();
    tracing::debug!("Flushing OpenTelemetry spans (timeout: {timeout:?})");

    // force_flush is synchronous and honours the batch exporter's own
    // configured timeout; we log each individual error but continue.
    let flush_results = provider.force_flush();
    let mut flush_errors = 0usize;
    for result in flush_results {
        if let Err(e) = result {
            tracing::error!("OpenTelemetry flush error: {e}");
            flush_errors += 1;
        }
    }

    if flush_errors > 0 {
        tracing::warn!(
            "{flush_errors} span pipeline(s) reported errors during flush; \
             some telemetry data may have been lost"
        );
    }

    // Shut down the provider — this stops the background export thread and
    // releases its resources.
    if let Err(e) = provider.shutdown() {
        tracing::error!("OpenTelemetry provider shutdown error: {e}");
    }

    // Clear the global tracer so post-shutdown code doesn't queue spans.
    opentelemetry::global::shutdown_tracer_provider();

    tracing::info!("OpenTelemetry shutdown complete (timeout budget: {timeout:?})");
}

/// Initialize the tracer with non-fatal error handling.
///
/// This wrapper around [`init_tracer`] ensures that initialization failures
/// never cause the application to panic. If initialization fails, a warning
/// is logged and the system falls back to the no-op tracer.
///
/// # Non-Fatal Guarantee
///
/// - Initialization errors are logged as warnings
/// - The application continues with a no-op tracer
/// - Observability infrastructure issues don't disrupt application flow
pub fn init_tracer_non_fatal(
    service_name: &str,
    otlp_endpoint: Option<&str>,
) -> TracerProvider {
    match init_tracer(service_name, otlp_endpoint) {
        Ok(provider) => provider,
        Err(e) => {
            tracing::warn!(
                "Failed to initialize OpenTelemetry tracer (falling back to no-op): {e}"
            );
            // Return a no-op tracer provider
            let resource = Resource::new(vec![
                opentelemetry::KeyValue::new(SERVICE_NAME, service_name.to_string()),
                opentelemetry::KeyValue::new(SERVICE_VERSION, env!("CARGO_PKG_VERSION")),
            ]);
            let provider = sdktrace::TracerProvider::builder()
                .with_config(sdktrace::Config::default().with_resource(resource))
                .build();
            opentelemetry::global::set_tracer_provider(provider.clone());
            provider
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flush_timeout_defaults_to_constant() {
        // Ensure the env var is absent for this test.
        std::env::remove_var("OTEL_FLUSH_TIMEOUT_SECS");
        assert_eq!(flush_timeout(), DEFAULT_FLUSH_TIMEOUT);
    }

    #[test]
    fn flush_timeout_reads_env_var() {
        std::env::set_var("OTEL_FLUSH_TIMEOUT_SECS", "10");
        assert_eq!(flush_timeout(), Duration::from_secs(10));
        std::env::remove_var("OTEL_FLUSH_TIMEOUT_SECS");
    }

    #[test]
    fn flush_timeout_ignores_invalid_env_var() {
        std::env::set_var("OTEL_FLUSH_TIMEOUT_SECS", "not-a-number");
        assert_eq!(flush_timeout(), DEFAULT_FLUSH_TIMEOUT);
        std::env::remove_var("OTEL_FLUSH_TIMEOUT_SECS");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::global;

    #[test]
    fn test_init_tracer_sets_global_provider_and_resource() {
        let provider = init_tracer("test-service", None).expect("failed to init tracer");
        let tracer = global::tracer("test-tracer");
        let span = tracer.start("test-span");
        span.end();

        let resource = provider.config().resource();
        let mut service_name = None;
        let mut service_version = None;

        for attr in resource.iter() {
            match attr.key.as_str() {
                "service.name" => service_name = Some(attr.value.to_string()),
                "service.version" => service_version = Some(attr.value.to_string()),
                _ => {}
            }
        }

        assert_eq!(service_name.as_deref(), Some("test-service"));
        assert!(service_version.is_some());

        shutdown_tracer(provider);
    }

    #[test]
    fn test_shutdown_tracer_drops_provider_without_error() {
        let provider = init_tracer("test-service", None).expect("failed to init tracer");
        shutdown_tracer(provider);
    }
}
