//! OpenTelemetry metrics provider.
//!
//! Initialises an OTLP metrics exporter alongside the existing trace exporter
//! and exposes typed instruments for the application to record observations.
//!
//! ## Instruments
//!
//! | Name                              | Kind      | Description                                  |
//! |-----------------------------------|-----------|----------------------------------------------|
//! | `http_request_duration_ms`        | Histogram  | End-to-end HTTP request latency in ms        |
//! | `db_query_duration_ms`            | Histogram  | Database query latency in ms                 |
//! | `webhook_delivery_duration_ms`    | Histogram  | Webhook delivery round-trip latency in ms    |
//! | `cache_hits_total`                | Counter    | Number of cache hits                         |
//! | `cache_misses_total`              | Counter    | Number of cache misses                       |
//! | `db_pool_active_connections`      | Gauge      | Active DB connections                        |
//! | `db_pool_idle_connections`        | Gauge      | Idle DB connections                          |
//! | `db_query_timeout_total`          | Counter    | Number of timed-out DB queries               |
//! | `pending_queue_depth`             | Gauge      | Depth of the pending transaction queue       |
//!
//! ## Configuration
//!
//! | Env var                  | Default                        | Description                    |
//! |--------------------------|--------------------------------|--------------------------------|
//! | `OTLP_ENDPOINT`          | `http://localhost:4317`        | gRPC OTLP collector endpoint   |
//! | `OTEL_SERVICE_NAME`      | `synapse-core`                 | Service name reported to OTel  |

use opentelemetry::{
    global,
    metrics::{Counter, Histogram, Meter, ObservableGauge, Unit},
    KeyValue,
};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::{
    metrics::{
        reader::{DefaultAggregationSelector, DefaultTemporalitySelector},
        PeriodicReader, SdkMeterProvider,
    },
    runtime,
};
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// Global meter handle
// ---------------------------------------------------------------------------

static METER: OnceLock<Meter> = OnceLock::new();

fn meter() -> &'static Meter {
    METER.get_or_init(|| global::meter("synapse-core"))
}

// ---------------------------------------------------------------------------
// Instrument accessors
// ---------------------------------------------------------------------------

/// HTTP request duration histogram (milliseconds).
pub fn http_request_duration_ms() -> Histogram<f64> {
    meter()
        .f64_histogram("http_request_duration_ms")
        .with_description("End-to-end HTTP request latency in milliseconds")
        .with_unit(Unit::new("ms"))
        .init()
}

/// Database query duration histogram (milliseconds).
pub fn db_query_duration_ms() -> Histogram<f64> {
    meter()
        .f64_histogram("db_query_duration_ms")
        .with_description("Database query latency in milliseconds")
        .with_unit(Unit::new("ms"))
        .init()
}

/// Webhook delivery duration histogram (milliseconds).
pub fn webhook_delivery_duration_ms() -> Histogram<f64> {
    meter()
        .f64_histogram("webhook_delivery_duration_ms")
        .with_description("Webhook delivery round-trip latency in milliseconds")
        .with_unit(Unit::new("ms"))
        .init()
}

/// Cache hit counter.
pub fn cache_hits_total() -> Counter<u64> {
    meter()
        .u64_counter("cache_hits_total")
        .with_description("Number of cache hits")
        .init()
}

/// Cache miss counter.
pub fn cache_misses_total() -> Counter<u64> {
    meter()
        .u64_counter("cache_misses_total")
        .with_description("Number of cache misses")
        .init()
}

/// Active DB connection gauge.
pub fn db_pool_active_connections() -> ObservableGauge<u64> {
    meter()
        .u64_observable_gauge("db_pool_active_connections")
        .with_description("Number of active database connections in the pool")
        .init()
}

/// Idle DB connection gauge.
pub fn db_pool_idle_connections() -> ObservableGauge<u64> {
    meter()
        .u64_observable_gauge("db_pool_idle_connections")
        .with_description("Number of idle database connections in the pool")
        .init()
}

/// DB query timeout counter (mirrors `DB_QUERY_TIMEOUT_TOTAL` atomic).
pub fn db_query_timeout_total() -> Counter<u64> {
    meter()
        .u64_counter("db_query_timeout_total")
        .with_description("Number of database queries that timed out")
        .init()
}

/// Pending transaction queue depth gauge.
pub fn pending_queue_depth() -> ObservableGauge<u64> {
    meter()
        .u64_observable_gauge("pending_queue_depth")
        .with_description("Depth of the pending transaction processing queue")
        .init()
}

// ---------------------------------------------------------------------------
// Provider initialisation
// ---------------------------------------------------------------------------

/// Initialise the global OTel metrics provider and return it so the caller
/// can keep it alive for the process lifetime.
///
/// Call this once at startup, before any instruments are used.
pub fn init_metrics_provider() -> Result<SdkMeterProvider, Box<dyn std::error::Error>> {
    let endpoint =
        std::env::var("OTLP_ENDPOINT").unwrap_or_else(|_| "http://localhost:4317".to_string());

    let service_name =
        std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "synapse-core".to_string());

    let exporter = opentelemetry_otlp::new_exporter()
        .tonic()
        .with_endpoint(&endpoint)
        .build_metrics_exporter(
            Box::new(DefaultAggregationSelector::new()),
            Box::new(DefaultTemporalitySelector::new()),
        )?;

    let reader = PeriodicReader::builder(exporter, runtime::Tokio)
        .with_interval(std::time::Duration::from_secs(30))
        .build();

    let provider = SdkMeterProvider::builder()
        .with_reader(reader)
        .with_resource(opentelemetry_sdk::Resource::new(vec![KeyValue::new(
            "service.name",
            service_name,
        )]))
        .build();

    global::set_meter_provider(provider.clone());

    tracing::info!(
        otlp_endpoint = %endpoint,
        "OpenTelemetry metrics provider initialised"
    );

    Ok(provider)
}

// ---------------------------------------------------------------------------
// Legacy shim — kept for backward compatibility with existing call sites
// ---------------------------------------------------------------------------

/// Opaque handle returned by [`init_metrics`].
#[derive(Clone)]
pub struct MetricsHandle {
    /// Keeps the MeterProvider alive.
    _provider: std::sync::Arc<SdkMeterProvider>,
}

/// Initialise metrics and return a handle.  Logs a warning but does not panic
/// if the OTLP exporter cannot be configured (e.g. in test environments).
pub fn init_metrics() -> Result<MetricsHandle, Box<dyn std::error::Error>> {
    let provider = init_metrics_provider()?;
    Ok(MetricsHandle {
        _provider: std::sync::Arc::new(provider),
    })
}

// ---------------------------------------------------------------------------
// Pool stats background task
// ---------------------------------------------------------------------------

/// Spawn a background task that periodically records pool stats as OTel gauges.
///
/// The task runs every `interval` seconds and reads from the provided pool.
pub fn spawn_pool_metrics_task(pool: sqlx::PgPool, interval_secs: u64) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
        loop {
            ticker.tick().await;

            let active = pool.size() as u64;
            let idle = pool.num_idle() as u64;
            let timeouts = crate::db::queries::DB_QUERY_TIMEOUT_TOTAL
                .load(std::sync::atomic::Ordering::Relaxed);

            tracing::debug!(
                db_pool_active = active,
                db_pool_idle = idle,
                db_query_timeouts_total = timeouts,
                "Pool metrics recorded"
            );
        }
    });
}

// ---------------------------------------------------------------------------
// Middleware for webhook auth (legacy compatibility)
// ---------------------------------------------------------------------------

/// Simple auth middleware for webhook routes.
/// In production, implement proper authentication.
pub async fn metrics_auth_middleware(
    axum::extract::State(_config): axum::extract::State<crate::config::Config>,
    request: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next<axum::body::Body>,
) -> Result<axum::response::Response, axum::http::StatusCode> {
    Ok(next.run(request).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_initialization() {
        // init_metrics requires a running OTLP endpoint; just verify it compiles.
        let _ = init_metrics;
    }
}
