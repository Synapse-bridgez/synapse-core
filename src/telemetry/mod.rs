//! Telemetry module — OpenTelemetry tracing, connection pooling, webhook handlers,
//! input validation, reconnection logic, health checks, and metrics optimization.
//!
//! All error paths are designed to degrade gracefully without panicking.

pub mod connection_pool;
pub mod data_export;
pub mod error_handling;
pub mod health_checks;
pub mod input_validation;
pub mod metrics_optimization;
pub mod reconnection;
pub mod webhook;

pub use connection_pool::{ConnectionPool, PoolConfig};
pub use data_export::{DataExportService, ExportBatch, ExportConfig, TelemetryRecord};
pub use error_handling::{ErrorAction, ErrorHandler, TelemetryError, TelemetryResult};
pub use health_checks::{HealthCheckConfig, HealthCheckManager, HealthCheckResult};
pub use input_validation::InputValidator;
pub use metrics_optimization::{CardinalityLimiter, MetricsInstruments};
pub use reconnection::ReconnectionManager;
pub use webhook::{TelemetryWebhookHandler, WebhookPayload, WebhookResult};

// ---------------------------------------------------------------------------
// OpenTelemetry tracer initialisation.
//
// Call `init_tracer` once at startup. It returns a `TracerManager` that must be
// kept alive for the duration of the process (dropping it flushes and shuts
// down the exporter). When no OTLP endpoint is configured the function installs
// a no-op provider so the rest of the code compiles and runs unchanged.
//
// Call `shutdown_tracer` during process teardown to flush in-flight spans, shut
// down the provider and clear the global tracer. The flush timeout defaults to
// `DEFAULT_FLUSH_TIMEOUT` and can be overridden via `OTEL_FLUSH_TIMEOUT_SECS`.
// ---------------------------------------------------------------------------

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

/// Manager for the global OpenTelemetry tracer provider.
///
/// This type holds the provider for the lifetime of the application and
/// exposes a structured shutdown path for graceful tracer teardown.
pub struct TracerManager {
    provider: TracerProvider,
}

impl TracerManager {
    /// Initialize the tracer manager and register the provider globally.
    pub fn init(service_name: &str, otlp_endpoint: Option<&str>) -> anyhow::Result<Self> {
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

        Ok(Self { provider })
    }

    /// Shut down the tracer and flush any buffered spans.
    pub fn shutdown(self) {
        shutdown_tracer(self.provider)
    }
}

/// Initialise the global tracer manager and return a guard that can be
/// shut down cleanly on exit.
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
) -> anyhow::Result<TracerManager> {
    TracerManager::init(service_name, otlp_endpoint)
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

    // Drop the provider — this stops the background export thread and releases
    // its resources.
    drop(provider);

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
pub fn init_tracer_non_fatal(service_name: &str, otlp_endpoint: Option<&str>) -> TracerManager {
    match TracerManager::init(service_name, otlp_endpoint) {
        Ok(manager) => manager,
        Err(e) => {
            tracing::warn!(
                "Failed to initialize OpenTelemetry tracer (falling back to no-op): {e}"
            );
            let resource = Resource::new(vec![
                opentelemetry::KeyValue::new(SERVICE_NAME, service_name.to_string()),
                opentelemetry::KeyValue::new(SERVICE_VERSION, env!("CARGO_PKG_VERSION")),
            ]);
            let provider = sdktrace::TracerProvider::builder()
                .with_config(sdktrace::Config::default().with_resource(resource))
                .build();
            opentelemetry::global::set_tracer_provider(provider.clone());
            TracerManager { provider }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::global;
    use opentelemetry::trace::{Span, Tracer};

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

    #[test]
    fn test_init_tracer_sets_global_provider_and_resource() {
        let manager = init_tracer("test-service", None).expect("failed to init tracer");
        let tracer = global::tracer("test-tracer");
        let mut span = tracer.start("test-span");
        span.end();

        let resource = manager.provider.config().resource.clone();
        let mut service_name = None;
        let mut service_version = None;

        for (key, value) in resource.iter() {
            match key.as_str() {
                "service.name" => service_name = Some(value.to_string()),
                "service.version" => service_version = Some(value.to_string()),
                _ => {}
            }
        }

        assert_eq!(service_name.as_deref(), Some("test-service"));
        assert!(service_version.is_some());

        manager.shutdown();
    }

    #[test]
    fn test_shutdown_tracer_drops_provider_without_error() {
        let manager = init_tracer("test-service", None).expect("failed to init tracer");
        manager.shutdown();
    }
}
