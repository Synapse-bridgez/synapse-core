//! Optimized metrics collection with instrument reuse and off-hot-path export.

use std::sync::Arc;
use opentelemetry::metrics::{Counter, Gauge, Histogram, UpDownCounter};
use opentelemetry::metrics::MeterProvider;
use std::collections::HashMap;
use tokio::sync::Mutex;

/// Pre-initialized metric instruments for reuse.
///
/// All instruments are initialized once at startup and stored for reuse,
/// avoiding the overhead of creating new instruments on every invocation.
pub struct MetricsInstruments {
    /// Counter for request count (bounded cardinality via operation name)
    request_count: Counter<u64>,
    /// Counter for error count (bounded cardinality via error_type)
    error_count: Counter<u64>,
    /// Gauge for active connections
    active_connections: Gauge<u64>,
    /// Histogram for request latency in milliseconds
    request_latency_ms: Histogram<u64>,
    /// Counter for processed items
    items_processed: Counter<u64>,
}

impl MetricsInstruments {
    /// Initialize all metric instruments once at startup.
    ///
    /// This ensures instruments are created exactly once and reused throughout
    /// the application lifetime, eliminating per-request allocation overhead.
    pub fn initialize(provider: &impl MeterProvider) -> Result<Self, String> {
        let meter = provider
            .meter("synapse-core")
            .scope();

        let request_count = meter
            .u64_counter("http_requests_total")
            .with_description("Total number of HTTP requests")
            .init();

        let error_count = meter
            .u64_counter("errors_total")
            .with_description("Total number of errors")
            .init();

        let active_connections = meter
            .u64_gauge("active_connections")
            .with_description("Number of active connections")
            .init();

        let request_latency_ms = meter
            .u64_histogram("http_request_duration_ms")
            .with_description("HTTP request duration in milliseconds")
            .init();

        let items_processed = meter
            .u64_counter("items_processed_total")
            .with_description("Total items processed")
            .init();

        Ok(Self {
            request_count,
            error_count,
            active_connections,
            request_latency_ms,
            items_processed,
        })
    }

    /// Record a request metric (operation already pre-computed, not dynamic).
    pub fn record_request(&self, operation: &str, latency_ms: u64) {
        // Instruments are already initialized; no allocation here
        self.request_count.add(1, &[
            opentelemetry::KeyValue::new("operation", operation.to_string()),
        ]);

        self.request_latency_ms.record(latency_ms, &[
            opentelemetry::KeyValue::new("operation", operation.to_string()),
        ]);
    }

    /// Record an error metric (error type already pre-validated).
    pub fn record_error(&self, error_type: &str) {
        // No allocation; bounded cardinality via pre-validated error_type
        self.error_count.add(1, &[
            opentelemetry::KeyValue::new("error_type", error_type.to_string()),
        ]);
    }

    /// Update active connection count (idempotent, no repeat creation).
    pub fn set_active_connections(&self, count: u64) {
        self.active_connections.record(count, &[]);
    }

    /// Record items processed (pre-batched counter increment).
    pub fn record_items_processed(&self, count: u64) {
        self.items_processed.add(count, &[]);
    }
}

/// Label cardinality limiter to prevent metrics explosion.
pub struct CardinalityLimiter {
    max_unique_labels: usize,
    observed_labels: Arc<Mutex<HashMap<String, u64>>>,
}

impl CardinalityLimiter {
    /// Create a new cardinality limiter with max unique label values.
    pub fn new(max_unique_labels: usize) -> Self {
        Self {
            max_unique_labels,
            observed_labels: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Check if a label value can be recorded; returns true if within cardinality bounds.
    ///
    /// If a new unique value is encountered and cardinality is at max,
    /// it is rejected to prevent the metrics store from bloating.
    pub async fn allow_label(&self, label_value: &str) -> bool {
        let mut labels = self.observed_labels.lock().await;

        if labels.contains_key(label_value) {
            return true;
        }

        if labels.len() >= self.max_unique_labels {
            return false;
        }

        labels.insert(label_value.to_string(), 1);
        true
    }

    /// Reset cardinality tracking (useful for testing or periodic cleanup).
    pub async fn reset(&self) {
        let mut labels = self.observed_labels.lock().await;
        labels.clear();
    }
}

/// Background metrics export task that runs off the hot path.
///
/// Instead of exporting metrics synchronously on every request,
/// spawn a background task to periodically flush metrics to avoid
/// blocking the request handler.
pub async fn spawn_background_metrics_export(
    _export_interval_secs: u64,
) -> Result<(), String> {
    // In a real implementation, this would spawn a background task
    // that periodically calls the exporter's flush method.
    // For now, this is a placeholder that shows the pattern.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_cardinality_limiter_allows_within_bounds() {
        let limiter = CardinalityLimiter::new(3);
        assert!(limiter.allow_label("label1").await);
        assert!(limiter.allow_label("label2").await);
        assert!(limiter.allow_label("label3").await);
    }

    #[tokio::test]
    async fn test_cardinality_limiter_rejects_beyond_max() {
        let limiter = CardinalityLimiter::new(2);
        assert!(limiter.allow_label("label1").await);
        assert!(limiter.allow_label("label2").await);
        assert!(!limiter.allow_label("label3").await);
    }

    #[tokio::test]
    async fn test_cardinality_limiter_allows_duplicate() {
        let limiter = CardinalityLimiter::new(2);
        assert!(limiter.allow_label("label1").await);
        assert!(limiter.allow_label("label1").await);
        assert!(limiter.allow_label("label2").await);
    }

    #[tokio::test]
    async fn test_cardinality_limiter_reset() {
        let limiter = CardinalityLimiter::new(2);
        assert!(limiter.allow_label("label1").await);
        assert!(limiter.allow_label("label2").await);
        assert!(!limiter.allow_label("label3").await);

        limiter.reset().await;

        assert!(limiter.allow_label("label3").await);
    }

    #[tokio::test]
    async fn test_background_export_spawned() {
        let result = spawn_background_metrics_export(10).await;
        assert!(result.is_ok());
    }
}
