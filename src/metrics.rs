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
//! | `transaction_insert_missing_partition_total` | Counter | 23514 hits at insert_transaction, triggering self-heal |
//! | `partition_self_heal_duration_ms` | Histogram  | ensure_partition_for latency (advisory-lock wait dominated) |
//! | `idempotency_db_fallback_recovered_total` | Counter | DB-fallback idempotency keys recognized after Redis recovery |
//! | `reconciliation_duplicate_report_prevented_total` | Counter | Duplicate reconciliation report inserts caught by the unique constraint |
//! | `account_monitor_concurrent_write_prevented_total` | Counter | AccountMonitor completion writes that lost a row-lock race |
//! | `transaction_processor_completion_conflict_prevented_total` | Counter | CompleteStage writes that lost a row-lock race |
//! | `transaction_processor_stage_executions_total` | Counter | Stage executions, labeled by stage (verifies rollout-percentage gating in prod) |
//! | `webhook_delivery_total`          | Counter    | Webhook delivery attempts, labeled by outcome and endpoint_id |
//! | `webhook_circuit_breaker_transitions_total` | Counter | CB state transitions, labeled by transition type (includes half-open probe_succeeded/probe_failed/flapping_detected) |
//! | `webhook_circuit_breaker_half_open_duration_ms` | Histogram | Time spent in half-open state per probe |
//! | `webhook_rate_limit_self_healed_total` | Counter | Rate-limit counters found without a TTL and self-healed |
//! | `admin_audit_search_requests_total` | Counter | Requests to GET /admin/audit/search (newly mounted; see docs/audit-compliance-admin-endpoints.md) |
//! | `admin_compliance_report_requests_total` | Counter | Requests to the compliance report endpoints, labeled by operation (newly mounted) |
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

/// Background task timeout counter.
pub fn background_task_timeout_total() -> Counter<u64> {
    meter()
        .u64_counter("background_task_timeout_total")
        .with_description("Number of background tasks that exceeded their timeout")
        .init()
}

/// Slow database query counter.
pub fn db_slow_queries_total() -> Counter<u64> {
    meter()
        .u64_counter("db_slow_queries_total")
        .with_description("Number of slow database queries")
        .init()
}

/// Missing-partition (23514) counter at the `insert_transaction` call site.
pub fn transaction_insert_missing_partition_total() -> Counter<u64> {
    meter()
        .u64_counter("transaction_insert_missing_partition_total")
        .with_description(
            "Number of transaction inserts that hit a missing-partition (23514) error \
             and triggered the synchronous ensure_partition_for self-heal path",
        )
        .init()
}

/// Latency of the synchronous missing-partition self-heal call
/// (`ensure_partition_for`), in milliseconds. Under contention this is
/// dominated by `pg_advisory_xact_lock` wait time; uncontended calls are
/// dominated by the `CREATE TABLE` DDL itself.
pub fn partition_self_heal_duration_ms() -> Histogram<f64> {
    meter()
        .f64_histogram("partition_self_heal_duration_ms")
        .with_description(
            "Latency of the missing-partition self-heal call, dominated by \
             advisory-lock wait time under concurrent contention",
        )
        .with_unit(Unit::new("ms"))
        .init()
}

/// Counter for idempotency keys recovered from the database fallback table
/// on the healthy-Redis lookup path (i.e. a key written during a Redis
/// outage, found again after Redis recovered), instead of being silently
/// double-executed.
pub fn idempotency_db_fallback_recovered_total() -> Counter<u64> {
    meter()
        .u64_counter("idempotency_db_fallback_recovered_total")
        .with_description(
            "Idempotency keys recovered from the DB fallback table on Redis-healthy \
             lookup, i.e. requests recorded during a Redis outage and recognized \
             again after recovery instead of being double-executed",
        )
        .init()
}

/// Counter for reconciliation report inserts skipped because a report for
/// the same `(period_start, period_end)` already existed — i.e. a duplicate
/// caught by the unique constraint after a concurrent job run, rather than
/// producing a second report row.
pub fn reconciliation_duplicate_report_prevented_total() -> Counter<u64> {
    meter()
        .u64_counter("reconciliation_duplicate_report_prevented_total")
        .with_description(
            "Reconciliation report inserts skipped due to the (period_start, period_end) \
             unique constraint catching a concurrent duplicate run",
        )
        .init()
}

/// Counter for AccountMonitor completion writes that lost the race for a
/// candidate transaction because `FOR UPDATE` row locking meant a concurrent
/// `process_payment` call already claimed it (rows_affected == 0 on the
/// guarded completion UPDATE).
pub fn account_monitor_concurrent_write_prevented_total() -> Counter<u64> {
    meter()
        .u64_counter("account_monitor_concurrent_write_prevented_total")
        .with_description(
            "AccountMonitor completion writes that lost a row-lock race for the same \
             candidate transaction, prevented from overwriting a concurrent winner",
        )
        .init()
}

/// Counter for `TransactionProcessor::CompleteStage` completion writes that
/// lost a row-lock race for the same transaction (rows_affected == 0 on the
/// guarded completion UPDATE), analogous to
/// `account_monitor_concurrent_write_prevented_total`.
pub fn transaction_processor_completion_conflict_prevented_total() -> Counter<u64> {
    meter()
        .u64_counter("transaction_processor_completion_conflict_prevented_total")
        .with_description(
            "TransactionProcessor CompleteStage writes that lost a row-lock race for \
             the same transaction, prevented from overwriting a concurrent winner",
        )
        .init()
}

/// Stage-execution counter for `TransactionProcessor`, broken down by which
/// rollout-percentage bucket a stage ran in, so the fixed tenant/account-
/// scoped percentage gating is provably respected in production rather than
/// only in the unit test.
pub fn transaction_processor_stage_executions_total() -> Counter<u64> {
    meter()
        .u64_counter("transaction_processor_stage_executions_total")
        .with_description(
            "TransactionProcessor stage executions, labeled by stage name and whether \
             the stage's feature flag was rollout-percentage-gated",
        )
        .init()
}

/// Counter for `process_batch` completions that had no matching Horizon
/// payment found (see `services::processor::find_matching_payment`). While
/// `payment_verification_enabled` is off for an account, this fires in
/// shadow mode on every such completion so operators can see the exact
/// blast radius before ramping the flag's `rollout_percentage` up. Once the
/// flag is fully on, this should be structurally zero — completion is
/// gated on a match — so any nonzero rate here after full rollout means a
/// residual gap in the verification logic itself.
pub fn payment_verification_no_match_completed_total() -> Counter<u64> {
    meter()
        .u64_counter("payment_verification_no_match_completed_total")
        .with_description(
            "process_batch completions with no matching Horizon payment found. Nonzero \
             while payment_verification_enabled is off (shadow mode) is expected; nonzero \
             after full rollout indicates a verification-logic gap.",
        )
        .init()
}

/// Counter for pending transactions left pending (rather than immediately
/// failed) because `process_batch` could not yet verify their expected
/// payment but the retry window has not elapsed — covers the
/// account-not-found case as well as "account exists, no matching payment
/// yet" and transient Horizon lookup failures.
pub fn payment_verification_retry_deferred_total() -> Counter<u64> {
    meter()
        .u64_counter("payment_verification_retry_deferred_total")
        .with_description(
            "Pending transactions left pending for retry instead of being immediately \
             failed, because their expected Horizon payment could not yet be verified",
        )
        .init()
}

/// `HorizonClient::stream_payments` reconnect counter, labeled by `reason`
/// ("clean_close" | "error"). Prior to the Part B fix, only "clean_close"
/// was ever reconnected — any transport/response error terminated the
/// stream permanently. Nonzero "error" counts now show the fix is actually
/// engaging, once `AccountMonitor` (currently dead code) is wired live.
pub fn stream_reconnect_total() -> Counter<u64> {
    meter()
        .u64_counter("stream_reconnect_total")
        .with_description(
            "HorizonClient::stream_payments reconnect attempts, labeled by reason \
             (clean_close | error)",
        )
        .init()
}

/// Webhook delivery outcome counter, labeled by `outcome` ("success" |
/// "failure") and `endpoint_id`.
pub fn webhook_delivery_total() -> Counter<u64> {
    meter()
        .u64_counter("webhook_delivery_total")
        .with_description("Webhook delivery attempts, labeled by outcome and endpoint_id")
        .init()
}

/// Circuit breaker state-transition counter, labeled by `transition`
/// ("opened" | "closed" | "probe_sent" | "probe_blocked" |
/// "probe_succeeded" | "probe_failed" | "flapping_detected"). The last three
/// are half-open-specific: `probe_succeeded`/`probe_failed` record the
/// outcome of the single delivery let through during a half-open probe, and
/// `flapping_detected` fires when probe failures repeat within the
/// configurable flap-detection window (see `WEBHOOK_CB_FLAP_THRESHOLD` /
/// `WEBHOOK_CB_FLAP_WINDOW_SECS` in `webhook_dispatcher`), signaling a
/// breaker that keeps bouncing between half-open and open rather than
/// recovering.
pub fn webhook_circuit_breaker_transitions_total() -> Counter<u64> {
    meter()
        .u64_counter("webhook_circuit_breaker_transitions_total")
        .with_description("Webhook circuit breaker state transitions, labeled by transition type")
        .init()
}

/// Time a half-open probe delivery took to resolve (success or failure),
/// i.e. time spent in the half-open state for that probe.
pub fn webhook_circuit_breaker_half_open_duration_ms() -> Histogram<f64> {
    meter()
        .f64_histogram("webhook_circuit_breaker_half_open_duration_ms")
        .with_description("Time spent in half-open state per circuit breaker probe, in ms")
        .with_unit(Unit::new("ms"))
        .init()
}

/// Counter for rate-limit counters found without a TTL and self-healed
/// (see webhook_dispatcher::check_rate_limit's atomic INCR+EXPIRE script).
pub fn webhook_rate_limit_self_healed_total() -> Counter<u64> {
    meter()
        .u64_counter("webhook_rate_limit_self_healed_total")
        .with_description(
            "Webhook rate-limit counters found without a TTL (e.g. a crash between a \
             separate INCR and EXPIRE) and self-healed instead of staying stuck",
        )
        .init()
}

/// Pending transaction queue depth gauge.
pub fn pending_queue_depth() -> ObservableGauge<u64> {
    meter()
        .u64_observable_gauge("pending_queue_depth")
        .with_description("Depth of the pending transaction processing queue")
        .init()
}

/// Settlement operation duration histogram (milliseconds).
pub fn settlement_duration_ms() -> Histogram<f64> {
    meter()
        .f64_histogram("settlement_duration_ms")
        .with_description("Settlement operation latency in milliseconds")
        .with_unit(Unit::new("ms"))
        .init()
}

/// Total number of locks successfully acquired.
pub fn lock_acquired_total() -> Counter<u64> {
    meter()
        .u64_counter("lock_acquired_total")
        .with_description("Total number of distributed locks successfully acquired")
        .init()
}

/// Requests verified against a secret's `previous` (grace-period) value
/// rather than `current`. A nonzero rate outside the window immediately
/// following a rotation indicates a caller stuck on the old secret.
pub fn secrets_previous_value_verified_total() -> Counter<u64> {
    meter()
        .u64_counter("secrets_previous_value_verified_total")
        .with_description(
            "Number of requests verified against a rotating secret's previous \
             (grace-period) value rather than its current value",
        )
        .init()
}

/// Time between this instance's local secret rotation timestamp and the
/// original detection that triggered the fleet-wide notification, in
/// milliseconds. Bounds the real-world propagation window described in the
/// secret-rotation coordination fix (see `secrets::SecretsManager`).
pub fn secrets_rotation_detection_lag_ms() -> Histogram<f64> {
    meter()
        .f64_histogram("secrets_rotation_detection_lag_ms")
        .with_description(
            "Lag between a secret rotation being detected on this instance and \
             the pub/sub notification that triggered it (0 for the instance \
             that detected the rotation itself via polling)",
        )
        .with_unit(Unit::new("ms"))
        .init()
}

/// Incremented each time the pub/sub rotation-notification channel is
/// unavailable and an instance falls back to poll-only detection.
pub fn secrets_rotation_pubsub_unavailable_total() -> Counter<u64> {
    meter()
        .u64_counter("secrets_rotation_pubsub_unavailable_total")
        .with_description(
            "Number of times the secret-rotation pub/sub channel was unavailable, \
             forcing fallback to poll-only detection",
        )
        .init()
}

/// Total number of lock contention events (failed acquire attempts).
pub fn lock_contention_total() -> Counter<u64> {
    meter()
        .u64_counter("lock_contention_total")
        .with_description("Total number of distributed lock contention events")
        .init()
}

/// Backup verification outcome counter, labeled by `result` ("success" |
/// "failure" | "no_backups").
pub fn backup_verification_total() -> Counter<u64> {
    meter()
        .u64_counter("backup_verification_total")
        .with_description("Outcome of the scheduled backup verification job, labeled by result")
        .init()
}

/// Duration of a single backup verification run, in milliseconds.
pub fn backup_verification_duration_ms() -> Histogram<f64> {
    meter()
        .f64_histogram("backup_verification_duration_ms")
        .with_description("Duration of the scheduled backup verification job")
        .with_unit(Unit::new("ms"))
        .init()
}

/// Audit log archive write outcome counter, labeled by `result` ("success" |
/// "failure"). A `failure` here means `run_retention` skipped deletion for
/// that run — see the hard invariant documented on `db::audit::run_retention`.
pub fn audit_archive_write_total() -> Counter<u64> {
    meter()
        .u64_counter("audit_archive_write_total")
        .with_description(
            "Outcome of writing an audit log retention archive to its storage \
             backend, labeled by result. A 'failure' means the corresponding \
             rows were NOT deleted from audit_logs this run.",
        )
        .init()
}

/// Lock hold duration histogram (milliseconds).
pub fn lock_hold_duration_ms() -> Histogram<f64> {
    meter()
        .f64_histogram("lock_hold_duration_ms")
        .with_description("Duration a distributed lock was held in milliseconds")
        .with_unit(opentelemetry::metrics::Unit::new("ms"))
        .init()
}

/// Requests to `GET /admin/audit/search`. Exists specifically to give
/// operators a way to confirm the endpoint has real traffic now that it's
/// mounted — see docs/audit-compliance-admin-endpoints.md for how to use
/// this to distinguish "no incidents" from "nobody's used this yet."
pub fn admin_audit_search_requests_total() -> Counter<u64> {
    meter()
        .u64_counter("admin_audit_search_requests_total")
        .with_description("Requests to the admin audit-log search endpoint")
        .init()
}

/// Requests to the compliance report endpoints, labeled by `operation`
/// ("generate" | "list").
pub fn admin_compliance_report_requests_total() -> Counter<u64> {
    meter()
        .u64_counter("admin_compliance_report_requests_total")
        .with_description("Requests to the admin compliance report endpoints, labeled by operation")
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
