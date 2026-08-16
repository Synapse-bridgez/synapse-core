use opentelemetry::metrics::MeterProvider as _;
use opentelemetry_sdk::metrics::{data::ResourceMetrics, SdkMeterProvider};
use opentelemetry_sdk::testing::metrics::InMemoryMetricsExporter;
use opentelemetry_sdk::{metrics::PeriodicReader, runtime};
use synapse_core::metrics::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build an isolated `SdkMeterProvider` backed by an `InMemoryMetricsExporter`
/// so that tests can call `force_flush` and then inspect exactly what was
/// recorded — without needing a live OTLP collector.
///
/// Using a local provider avoids touching the global `OnceLock<Meter>` that
/// the production code uses, so tests are fully isolated from each other.
fn local_provider() -> (SdkMeterProvider, InMemoryMetricsExporter) {
    let exporter = InMemoryMetricsExporter::default();
    let reader = PeriodicReader::builder(exporter.clone(), runtime::Tokio).build();
    let provider = SdkMeterProvider::builder().with_reader(reader).build();
    (provider, exporter)
}

/// Flush the provider, then return the most recently exported
/// `ResourceMetrics` snapshot.
fn flush_and_collect(
    provider: &SdkMeterProvider,
    exporter: &InMemoryMetricsExporter,
) -> ResourceMetrics {
    provider.force_flush().expect("force_flush failed");
    let mut batches = exporter
        .get_finished_metrics()
        .expect("get_finished_metrics failed");
    assert!(!batches.is_empty(), "no metric batches were exported");
    // Return the most-recent batch.
    batches.pop().unwrap()
}

/// Find the first `Metric` whose name matches `name` across all scopes.
fn find_metric<'a>(
    rm: &'a ResourceMetrics,
    name: &str,
) -> Option<&'a opentelemetry_sdk::metrics::data::Metric> {
    rm.scope_metrics
        .iter()
        .flat_map(|sm| sm.metrics.iter())
        .find(|m| m.name == name)
}

// ---------------------------------------------------------------------------
// Existing registration smoke-test (unchanged)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_metric_registration() {
    let handle = init_metrics().expect("Failed to initialize metrics");
    let _ = handle; // Verify handle is created successfully
}

// ---------------------------------------------------------------------------
// Counter — assert the cumulative sum increases after `add()`
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_counter_increment() {
    let (provider, exporter) = local_provider();
    let meter = provider.meter("test");

    let counter = meter.u64_counter("cache_hits_total").init();

    // Record two increments; the SDK accumulates them into a single cumulative sum.
    counter.add(1, &[]);
    counter.add(4, &[]);

    let rm = flush_and_collect(&provider, &exporter);
    let metric = find_metric(&rm, "cache_hits_total")
        .expect("cache_hits_total metric not found after recording");

    let sum = metric
        .data
        .as_any()
        .downcast_ref::<opentelemetry_sdk::metrics::data::Sum<u64>>()
        .expect("expected Sum<u64> for a u64 counter");

    assert!(!sum.data_points.is_empty(), "no data points recorded");
    let total: u64 = sum.data_points.iter().map(|dp| dp.value).sum();
    assert_eq!(
        total, 5,
        "expected cumulative sum of 5 (1 + 4), got {total}"
    );
}

// ---------------------------------------------------------------------------
// Histogram — assert observation count and sum after `record()`
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_histogram_recording() {
    let (provider, exporter) = local_provider();
    let meter = provider.meter("test");

    let histogram = meter.f64_histogram("http_request_duration_ms").init();

    histogram.record(10.0, &[]);
    histogram.record(20.0, &[]);
    histogram.record(30.0, &[]);

    let rm = flush_and_collect(&provider, &exporter);
    let metric = find_metric(&rm, "http_request_duration_ms")
        .expect("http_request_duration_ms metric not found after recording");

    let hist = metric
        .data
        .as_any()
        .downcast_ref::<opentelemetry_sdk::metrics::data::Histogram<f64>>()
        .expect("expected Histogram<f64>");

    assert!(
        !hist.data_points.is_empty(),
        "no histogram data points recorded"
    );
    let dp = &hist.data_points[0];
    assert_eq!(dp.count, 3, "expected 3 observations, got {}", dp.count);
    assert!(
        (dp.sum - 60.0_f64).abs() < f64::EPSILON,
        "expected sum 60.0, got {}",
        dp.sum
    );
}

// ---------------------------------------------------------------------------
// Gauge — assert the callback-reported value appears in the export
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_gauge_updates() {
    let (provider, exporter) = local_provider();
    let meter = provider.meter("test");

    // Observable gauge: the value is produced by a registered callback that
    // the SDK invokes during each collection cycle (i.e. on force_flush).
    let gauge = meter
        .u64_observable_gauge("db_pool_active_connections")
        .init();

    meter
        .register_callback(&[gauge.as_any()], move |observer| {
            observer.observe_u64(&gauge, 42, &[]);
        })
        .expect("failed to register gauge callback");

    let rm = flush_and_collect(&provider, &exporter);
    let metric = find_metric(&rm, "db_pool_active_connections")
        .expect("db_pool_active_connections metric not found after recording");

    let gauge_data = metric
        .data
        .as_any()
        .downcast_ref::<opentelemetry_sdk::metrics::data::Gauge<u64>>()
        .expect("expected Gauge<u64>");

    assert!(
        !gauge_data.data_points.is_empty(),
        "no gauge data points recorded"
    );
    assert_eq!(
        gauge_data.data_points[0].value, 42,
        "expected gauge value 42, got {}",
        gauge_data.data_points[0].value
    );
}

#[tokio::test]
#[ignore = "Middleware testing requires complex setup with axum 0.6"]
async fn test_metrics_authentication() {
    // Test disabled - requires Next::new which doesn't exist in axum 0.6
    // TODO: Rewrite this test for axum 0.6 compatibility
}

#[tokio::test]
async fn test_metrics_handle_clone() {
    let handle = init_metrics().expect("Failed to initialize metrics");
    let _cloned = handle.clone();
    // Verify cloning works for MetricsHandle
}
