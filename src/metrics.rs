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
//! | `readiness_initialization_duration_ms` | Histogram | Time spent in `run_initialization_checks`, labeled by outcome (ready/failed) |
//! | `settlement_transactions_total`   | Counter    | Transactions settled via settle_asset, labeled by asset_code |
//! | `redis_degraded_operations_total` | Counter | Redis failures degraded around, labeled by component and fallback (see `cache::degradation`) |
//! | `secrets_cached_fallback_used_total` | Counter | Verifications served from the last-known-good secret during a Vault outage |
//! | `secrets_stale_rejected_total` | Counter | Verifications refused because the cached secret passed its max fallback age |
//! | `vault_refresh_failures_total` | Counter | Failed Vault secret refreshes, labeled by secret |
//! | `vault_secret_staleness_seconds` | Gauge | Seconds since each secret's last successful Vault refresh |
//! | `vault_fallback_active` | Gauge | 1 while any secret is served from the Vault fallback cache |
//! | `dependency_scorecard_*` | Gauge | Rolling 7/30/90-day dependency uptime / error rate / p95 (see `services::dependency_scorecard`) |
//! | `tenant_request_latency_window_*` | Gauge | Per-tenant rolling-window latency histograms, top-K tenants only (see `tenant::latency`) |
//! | `admin_compliance_report_requests_total` | Counter | Requests to the compliance report endpoints, labeled by operation (newly mounted) |
//! | `readiness_initialization_duration_ms` | Histogram | Time spent in `run_initialization_checks`, labeled by outcome (ready/failed) |
//! | `settlement_transactions_total`   | Counter    | Transactions settled via settle_asset, labeled by asset_code |
//! | `tokio_tasks_live` / `tokio_tasks_load` | Gauge | Live tagged tokio tasks and the load explaining them, by category (task leak detection) |
//! | `tokio_runtime_alive_tasks`       | Gauge      | All alive tasks on the runtime (tagged or not) |
//! | `tokio_task_leak_suspected_total` | Counter    | Load-uncorrelated task growth detections, by category |
//! | `pipeline_stage_latency_ms`       | Histogram  | Latency attributed to each pipeline stage (latency budget) |
//! | `pipeline_stage_
//! | `tokio_tasks_live` / `tokio_tasks_load` | Gauge | Live tagged tokio tasks and the load explaining them, by category (task leak detection) |
//! | `tokio_runtime_alive_tasks`       | Gauge      | All alive tasks on the runtime (tagged or not) |
//! | `tokio_task_leak_suspected_total` | Counter    | Load-uncorrelated task growth detections, by category |
//! | `pipeline_stage_latency_ms`       | Histogram  | Latency attributed to each pipeline stage (latency budget) |
//! | `pipeline_stage_budget_utilization` | Gauge    | Stage P95 / stage budget, by stage |
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
// Tokio task leak detection
// ---------------------------------------------------------------------------
//
// Long-running services spawn many background tasks (scheduler jobs, webhook
// dispatch workers, WebSocket connection handlers). A task that never
// terminates (e.g. an unbounded channel receiver that is never dropped) leaks
// gradually and is easy to miss until it causes resource exhaustion.
//
// To make this class of bug observable we tag every spawned task with a
// [`TaskCategory`] at spawn time, track the live count per category, and
// correlate that count against a per-category *load reference* (active
// connections, in-flight jobs). A healthy task pool scales with load; a leak
// grows without bound while load stays flat, which is what we alert on.

/// Origin/category of a spawned tokio task.
///
/// Tagging at spawn time lets the resulting metric distinguish, e.g.,
/// WebSocket-connection tasks from scheduler-job tasks instead of reporting
/// one opaque total count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskCategory {
    /// One task per live WebSocket connection.
    WebSocketConnection,
    /// One task per scheduler job execution.
    SchedulerJob,
    /// One task per in-flight webhook dispatch.
    WebhookDispatch,
}

impl TaskCategory {
    /// Stable label value used on the exported metrics.
    pub fn as_str(self) -> &'static str {
        match self {
            TaskCategory::WebSocketConnection => "websocket-connection",
            TaskCategory::SchedulerJob => "scheduler-job",
            TaskCategory::WebhookDispatch => "webhook-dispatch",
        }
    }

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

/// Maximum configured DB pool size gauge.
pub fn db_pool_max_connections() -> ObservableGauge<u64> {
    meter()
        .u64_observable_gauge("db_pool_max_connections")
        .with_description("Configured maximum size of the database pool")
        .init()
}

/// Database pool saturation ratio: active connections divided by configured max.
pub fn db_pool_saturation_ratio() -> ObservableGauge<f64> {
    meter()
        .f64_observable_gauge("db_pool_saturation_ratio")
        .with_description("Fraction of the configured database pool currently in use")
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

/// Per-category live task count and load reference.
#[derive(Debug, Default, Clone, Copy)]
struct TaskCategoryState {
    live: u64,
    load: u64,
}

/// Number of WebSocket connections still open at the moment a drain
/// (`POST /admin/drain`) began. A single observation is recorded per drain.
pub fn ws_drain_connections_open_at_start() -> Histogram<f64> {
    meter()
        .f64_histogram("ws_drain_connections_open_at_start")
        .with_description("WebSocket connections still open when a drain began")
        .init()
}

/// Wall-clock duration of a drain, from `start_drain` to process exit, in
/// milliseconds.
pub fn ws_drain_duration_ms() -> Histogram<f64> {
    meter()
        .f64_histogram("ws_drain_duration_ms")
        .with_description("Time from drain start to process exit")
        .with_unit(Unit::new("ms"))
        .init()
}

/// WebSocket connections closed during a drain, labeled by `outcome`:
/// `"clean"` (closed itself in response to the drain signal before the
/// deadline) or `"forced"` (still open when the drain timeout elapsed and
/// the process exited anyway). A `forced` count above zero on a routine
/// deployment indicates connections are not draining within the configured
/// window and is worth alerting on.
pub fn ws_drain_connections_closed_total() -> Counter<u64> {
    meter()
        .u64_counter("ws_drain_connections_closed_total")
        .with_description(
            "WebSocket connections closed during drain, labeled by outcome \
             (clean = closed before the deadline, forced = still open when \
             the drain timeout elapsed)",
        )
        .init()
}

/// Compliance-classified report export events, labeled by `report_type`
/// (e.g. `"compliance_report"`, `"reconciliation_report"`). Distinct from
/// the routine `admin_*_report_requests_total` counters so a compliance
/// export is never conflated with a routine one in dashboards or alerts.
pub fn compliance_export_events_total() -> Counter<u64> {
    meter()
        .u64_counter("compliance_export_events_total")
        .with_description(
            "Compliance-classified report exports, labeled by report_type, \
             kept distinct from routine export telemetry",
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

fn task_state() -> &'static [std::sync::Mutex<TaskCategoryState>; 3] {
    TASK_STATE.get_or_init(|| {
        [
            std::sync::Mutex::new(TaskCategoryState::default()),
            std::sync::Mutex::new(TaskCategoryState::default()),
            std::sync::Mutex::new(TaskCategoryState::default()),
        ]
    })
}

fn category_index(category: TaskCategory) -> usize {
    match category {
        TaskCategory::WebSocketConnection => 0,
        TaskCategory::SchedulerJob => 1,
        TaskCategory::WebhookDispatch => 2,
    }
}

/// Record that a task of `category` has been spawned.
///
/// Call this immediately before `tokio::spawn`; pair it with
/// [`task_finished`] in the task body (or via [`TaskLeakGuard`]) so the live
/// count is decremented when the task terminates.
pub fn task_spawned(category: TaskCategory) {
    let mut state = task_state()[category_index(category)].lock().unwrap();
    state.live = state.live.saturating_add(1);
}

/// Record that a task of `category` has terminated.
pub fn task_finished(category: TaskCategory) {
    let mut state = task_state()[category_index(category)].lock().unwrap();
    state.live = state.live.saturating_sub(1);
}

/// Update the load reference for `category` (active connections, in-flight
/// jobs, ...). Used to correlate task count against legitimate traffic.
pub fn task_load_reference(category: TaskCategory, load: u64) {
    let mut state = task_state()[category_index(category)].lock().unwrap();
    state.load = load;
}

/// RAII guard that decrements the live task count when dropped.
///
/// Wrap the body of a spawned task so the count is decremented even if the
/// task panics or returns early:
///
/// ```ignore
/// metrics::task_spawned(TaskCategory::SchedulerJob);
/// tokio::spawn(async move {
///     let _guard = metrics::TaskLeakGuard::new(TaskCategory::SchedulerJob);
///     // ... job body ...
/// });
/// ```
pub struct TaskLeakGuard {
    category: TaskCategory,
}

impl TaskLeakGuard {
    /// Create a guard for `category`.
    pub fn new(category: TaskCategory) -> Self {
        Self { category }
    }
}

impl Drop for TaskLeakGuard {
    fn drop(&mut self) {
        task_finished(self.category);
    }
}

/// Snapshot of `(live, load)` for a category, for tests and alerting.
pub fn task_snapshot(category: TaskCategory) -> (u64, u64) {
    let state = task_state()[category_index(category)].lock().unwrap();
    (state.live, state.load)
}

/// A category's task count is considered leaked when it exceeds the load
/// reference by more than this many tasks *and* by more than this ratio.
const LEAK_ABSOLUTE_SLACK: u64 = 32;
const LEAK_RATIO_SLACK: f64 = 2.0;

/// Evaluate whether `category`'s live task count has grown without bound
/// relative to its load reference.
///
/// Raw count alone is not a leak signal (it scales with legitimate traffic),
/// so we only flag growth that is uncorrelated with load: the live count must
/// exceed both an absolute slack and a multiple of the load reference.
///
pub fn task_leak_suspected(category: TaskCategory) -> bool {
    let (live, load) = task_snapshot(category);
    let threshold = (load as f64 * LEAK_RATIO_SLACK) as u64 + LEAK_ABSOLUTE_SLACK;
    live > threshold
}

/// Register the tokio task-leak observable gauges on `meter`.
///
/// Exposes `tokio_spawned_tasks` (live count per category),
/// `tokio_task_load_reference` (load per category) and
/// `tokio_task_leak_suspected_total` (alert counter).
fn register_task_leak_gauges(meter: &Meter) {
    let spawned = meter
        .u64_observable_gauge("tokio_spawned_tasks")
        .with_description("Currently-live spawned tokio tasks, by category")
        .with_unit(Unit::new("{task}"))
        .with_callback(|observer| {
            for category in TaskCategory::ALL {
                let (live, _) = task_snapshot(category);
                observer.observe(live, &[KeyValue::new("category", category.as_str())]);
            }
        })
        .build();

    let load = meter
        .u64_observable_gauge("tokio_task_load_reference")
        .with_description("Load reference per task category (active connections / in-flight jobs)")
        .with_unit(Unit::new("{unit}"))
        .with_callback(|observer| {
            for category in TaskCategory::ALL {
                let (_, load) = task_snapshot(category);
                observer.observe(load, &[KeyValue::new("category", category.as_str())]);
            }
        })
        .init();

    (active_gauge, limit_gauge)
}

/// Settlement operation duration histogram (milliseconds).
pub fn settlement_duration_ms() -> Histogram<f64> {
    meter()
        .f64_histogram("settlement_duration_ms")
        .with_description("Settlement operation latency in milliseconds")
        .with_unit(Unit::new("ms"))
        .init()
}

/// Total transactions settled, labeled by `asset_code` (bounded — see
/// `docs/metrics-cardinality-convention.md`). Deliberately a counter added
/// by batch size rather than a per-call label: a raw per-call transaction
/// count used as a label value (as opposed to the metric's numeric value)
/// creates one time series per distinct count seen, which is unbounded.
pub fn settlement_transactions_total() -> Counter<u64> {
    meter()
        .u64_counter("settlement_transactions_total")
        .with_description("Total transactions settled via settle_asset, labeled by asset_code")
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

ounter("admin_audit_search_requests_total")
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

/// Replication lag measurement histogram (milliseconds), labeled by `replica`.
/// A value of -1 indicates the replica is unreachable.
pub fn replica_lag_ms() -> Histogram<f64> {
    meter()
        .f64_histogram("replica_lag_ms")
        .with_description("Replication lag on read replicas in milliseconds, labeled by replica name")
        .with_unit(crate::metrics::Unit::new("ms"))
        .init()
}

/// Replica lag alert counter, labeled by `replica` and `reason` ("threshold_exceeded" | "unreachable").
pub fn replica_lag_alert_total() -> Counter<u64> {
    meter()
        .u64_counter("replica_lag_alert_total")
        .with_description("Alerts triggered when replica lag exceeds threshold or replica becomes unreachable")
        .init()
}

/// ANALYZE staleness ratio histogram (0.0-1.0), labeled by `table` (schema.table).
/// Ratio = n_mod_since_analyze / estimate_live_rows.
pub fn analyze_staleness_ratio() -> Histogram<f64> {
    meter()
        .f64_histogram("analyze_staleness_ratio")
        .with_description("Ratio of modifications since last ANALYZE relative to estimated live rows")
        .init()
}

/// Count of tables flagged as stale (n_mod_since_analyze exceeding configured threshold).
pub fn stale_tables_total() -> Counter<u64> {
    meter()
        .u64_counter("stale_tables_total")
        .with_description("Number of tables with stale ANALYZE statistics relative to write volume")
        .init()
}

/// Table bloat ratio as a percentage, labeled by `schema` and `table`
pub fn table_bloat_ratio() -> Histogram<f64> {
    meter()
        .f64_histogram("table_bloat_ratio")
        .with_description("Estimated table bloat ratio as percentage of wasted space, labeled by schema and table")
        .init()
}

/// Estimated table bloat size in megabytes, labeled by `schema` and `table`
pub fn table_bloat_size_mb() -> Histogram<f64> {
    meter()
        .f64_histogram("table_bloat_size_mb")
        .with_description("Estimated table bloat size in MB, labeled by schema and table")
        .init()
}

/// Registers the observable gauges for tokio task leak detection
/// (`src/telemetry/task_leak.rs`): `tokio_tasks_live{category}` and
/// `tokio_tasks_load{category}` from the tagged-spawn registry, plus
/// `tokio_runtime_alive_tasks` from tokio's own runtime metrics, which also
/// counts untagged tasks. Call once at startup from inside the runtime and
/// keep the returned gauges alive.
pub fn register_task_leak_gauges() -> Vec<ObservableGauge<u64>> {
    use crate::telemetry::task_leak;

 
/// Registers the observable gauges for tokio task leak detection
/// (`src/telemetry/task_leak.rs`): `tokio_tasks_live{category}` and
/// `tokio_tasks_load{category}` from the tagged-spawn registry, plus
/// `tokio_runtime_alive_tasks` from tokio's own runtime metrics, which also
/// counts untagged tasks. Call once at startup from inside the runtime and
/// keep the returned gauges alive.
pub fn register_task_leak_gauges() -> Vec<ObservableGauge<u64>> {
    use crate::telemetry::task_leak;

    let live = meter()
        .u64_observable_gauge("tokio_tasks_live")
        .with_description("Live tokio tasks per spawn category")
        .with_callback(|observer| {
            for s in task_leak::global().snapshots() {
                observer.observe(s.live, &[KeyValue::new("category", s.category.as_str())]);
            }
        })
        .init();

    let load = meter()
        .u64_observable_gauge("tokio_tasks_load")
        .with_description(
            "Load that should explain each category's task count \
             (open WebSocket connections, registered scheduler jobs)",
        )
        .with_callback(|observer| {
            for s in task_leak::global().snapshots() {
                observer.observe(s.load, &[KeyValue::new("category", s.category.as_str())]);
            }
        })
        .init();

    let spawned = meter()
        .u64_observable_gauge("tokio_tasks_spawned_total")
        .with_description("Tasks ever spawned per category (monotonic)")
        .with_callback(|observer| {
            for s in task_leak::global().snapshots() {
                observer.observe(
                    s.spawned_total,
                    &[KeyValue::new("category", s.category.as_str())],
                );
            }
        })
        .init();

    let runtime = tokio::runtime::Handle::current();
    let alive = meter()
        .u64_observable_gauge("tokio_runtime_alive_tasks")
        .with_description("All alive tasks on the runtime, tagged or not")
        .with_callback(move |observer| {
            observer.observe(runtime.metrics().num_alive_tasks() as u64, &[]);
        })
        .init();

    vec![live, load, spawned, alive]
}

/// Times [`crate::telemetry::task_leak::LeakDetector`] flagged a category,
/// labeled by `category`.
pub fn tokio_task_leak_suspected_total() -> Counter<u64> {
    meter()
        .u64_counter("tokio_task_leak_suspected_total")
        .with_description("Load-uncorrelated task growth detections, by category")
        .init()
}

/// Per-transaction latency attributed to one pipeline stage, labeled by
/// `stage` (`src/telemetry/latency_budget.rs`). For the periodic stages
/// (settlement, reconciliation) each observation is one run's expected
/// per-transaction contribution: run duration plus mean wait for the run.
pub fn pipeline_stage_latency_ms() -> Histogram<f64> {
    meter()
        .f64_histogram("pipeline_stage_latency_ms")
        .with_description("Latency attributed to each pipeline stage")
        .with_unit(Unit::new("ms"))
        .init()
}

/// Registers `pipeline_stage_latency_p95_ms`, `pipeline_stage_budget_ms` and
/// `pipeline_stage_budget_utilization` (P95 / budget), each labeled by
/// `stage`, read from the latest latency-budget evaluation. Keep the returned
/// gauges alive.
pub fn register_latency_budget_gauges() -> Vec<ObservableGauge<f64>> {
    use crate::telemetry::latency_budget;

    let p95 = meter()
        .f64_observable_gauge("pipeline_stage_latency_p95_ms")
        .with_description("P95 stage latency in the last evaluation window")
        .with_unit(Unit::new("ms"))
        .with_callback(|observer| {
            for r in latency_budget::global().latest() {
                if let Some(p95) = r.p95_ms {
                    observer.observe(p95, &[KeyValue::new("stage", r.stage.as_str())]);
                }
            }
        })
        .init();

    let budget = meter()
        .f64_observable_gauge("pipeline_stage_budget_ms")
        .with_description("Configured latency budget per stage")
        .with_unit(Unit::new("ms"))
        .with_callback(|observer| {
            for r in latency_budget::global().latest() {
                observer.observe(r.budget_ms, &[KeyValue::new("stage", r.stage.as_str())]);
            }
        })
        .init();

    let utilization = meter()
        .f64_observable_gauge("pipeline_stage_budget_utilization")
        .with_description("P95 stage latency divided by its budget (>1 = over budget)")
        .with_callback(|observer| {
            for r in latency_budget::global().latest() {
                if let Some(u) = r.utilization() {
                    observer.observe(u, &[KeyValue::new("stage", r.stage.as_str())]);
                }
            }
        })
        .init();

    vec![p95, budget, utilization]
}

/// Redis failures a component degraded around instead of failing the
/// request, labeled by `component` and `fallback` (both closed sets — see
/// `cache::degradation`). Every Redis-dependent path emits this identically,
/// so `sum by (component)` during an outage is its full blast radius.
pub fn redis_degraded_operations_total() -> Counter<u64> {
    meter()
        .u64_counter("redis_degraded_operations_total")
        .with_description(
            "Redis failures degraded around (not failed), labeled by component and fallback",
        )
        .init()
}

/// Verifications served from a cached (last-known-good) secret while Vault
/// is unreachable, labeled by `secret`. Nonzero means the bounded Vault
/// fallback is live — never a silent state.
pub fn secrets_cached_fallback_used_total() -> Counter<u64> {
    meter()
        .u64_counter("secrets_cached_fallback_used_total")
        .with_description(
            "Secret verifications served from the last-known-good cache while Vault is \
             unreachable, labeled by secret",
        )
        .init()
}

/// Verifications refused because the cached secret outlived
/// `VAULT_SECRET_FALLBACK_MAX_AGE_SECS` during a Vault outage (or the secret
/// is not approved for caching), labeled by `secret`.
pub fn secrets_stale_rejected_total() -> Counter<u64> {
    meter()
        .u64_counter("secrets_stale_rejected_total")
        .with_description(
            "Secret verifications refused because the cached secret exceeded its maximum \
             fallback age during a Vault outage, labeled by secret",
        )
        .init()
}

/// Failed Vault secret refresh attempts, labeled by `secret`.
pub fn vault_refresh_failures_total() -> Counter<u64> {
    meter()
        .u64_counter("vault_refresh_failures_total")
        .with_description("Failed Vault secret refresh attempts, labeled by secret")
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
        st
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

    let leak_counter = meter
        .u64_counter("tokio_task_leak_suspected_total")
        .with_description("Times a category's task count grew without bound relative to load")
        .build();

    // Keep the instruments alive for the lifetime of the process; the SDK
    // holds the callbacks, but we retain the handles so they are not dropped.
    let _ = (spawned, load, leak_counter);
}

/// Evaluate every category and increment `tokio_task_leak_suspected_total`
/// for any that look leaked. Intended to be called periodically (e.g. from a
/// background watchdog task).
///
/// Returns the categories flagged as suspected leaks.
pub fn check_task_leaks() -> Vec<TaskCategory> {
    let mut flagged = Vec::new();
    for category in TaskCategory::ALL {
        if task_leak_suspected(category) {
            flagged.push(category);
        }
    }
    if !flagged.is_empty() {
        let counter = meter().u64_counter("tokio_task_leak_suspected_total").build();
        for category in &flagged {
            counter.add(1, &[KeyValue::new("category", category.as_str())]);
        }
    }
    flagged
}

// ---------------------------------------------------------------------------
// Per-release reliability scorecard
// ---------------------------------------------------------------------------
//
// Compares key reliability metrics (error rate, p50/p95/p99 latency, incident
// count) for a window *before* a release against an equivalent window *after*
// it, so regressions introduced by a specific release are caught and attributed
// quickly. This is reporting only; it does not trigger rollbacks (see issue 40).
//
// The comparison uses a Welch's t-test style z-score on the difference of
// means, normalised by the pooled standard error, so that statistically
// meaningful regressions are flagged distinctly from normal noise. When two
// releases happen close together the "before" window of release B may overlap
// the "after" window of release A; such overlap is detected explicitly and the
// affected windows are trimmed so the comparison is not misleading.

/// A single reliability metric observed over a comparison window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MetricSample {
    /// Mean value of the metric over the window.
    pub mean: f64,
    /// Standard deviation of the metric over the window.
    pub std_dev: f64,
    /// Number of observations contributing to the window.
    pub count: u64,
}

impl MetricSample {
    /// Construct a sample, clamping the count to at least 1 so downstream
    /// statistics never divide by zero.
    pub fn new(mean: f64, std_dev: f64, count: u64) -> Self {
        Self {
            mean,
            std_dev: std_dev.max(0.0),
            count: count.max(1),
        }
    }
}

/// The set of reliability metrics captured for one side of the comparison.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReliabilityWindow {
    /// Error rate as a fraction in `[0, 1]`.
    pub error_rate: MetricSample,
    /// p50 latency in milliseconds.
    pub p50_latency_ms: MetricSample,
    /// p95 latency in milliseconds.
    pub p95_latency_ms: MetricSample,
    /// p99 latency in milliseconds.
    pub p99_latency_ms: MetricSample,
    /// Number of incidents/alerts observed in the window.
    pub incident_count: MetricSample,
}

/// How a metric changed between the before and after windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegressionVerdict {
    /// The change is within normal noise.
    WithinNoise,
    /// A statistically meaningful regression (metric got worse).
    Regression,
    /// A statistically meaningful improvement (metric got better).
    Improvement,
}

/// The verdict for a single metric in the scorecard.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MetricComparison {
    /// Metric name, e.g. `error_rate` or `p95_latency_ms`.
    pub name: &'static str,
    /// Mean value before the release.
    pub before: f64,
    /// Mean value after the release.
    pub after: f64,
    /// Relative change `(after - before) / before`, or `0.0` when `before == 0`.
    pub relative_change: f64,
    /// Absolute z-score of the difference of means.
    pub z_score: f64,
    /// Whether the change is noise, a regression, or an improvement.
    pub verdict: RegressionVerdict,
}

/// The full per-release reliability scorecard.
#[derive(Debug, Clone, PartialEq)]
pub struct ReliabilityScorecard {
    /// Release identifier the scorecard was generated for.
    pub release: String,
    /// Per-metric comparisons.
    pub comparisons: Vec<MetricComparison>,
    /// `true` when the before/after windows overlapped a neighbouring rel

/* … truncated 11040 chars — edit only what you need near the top … */
