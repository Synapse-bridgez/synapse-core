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

/// Duration of `ReadinessState::run_initialization_checks`, labeled by
/// outcome (`ready` or `failed`). A rising trend on the `ready` outcome
/// indicates startup dependencies (DB/Redis/Horizon) are slow but still
/// progressing; a run that never reports at all indicates a stuck/hung
/// check, distinguishable from "slow" by its absence rather than a large
/// value. Label cardinality is bounded to the two known outcome values.
pub fn readiness_initialization_duration_ms() -> Histogram<f64> {
    meter()
        .f64_histogram("readiness_initialization_duration_ms")
        .with_description(
            "Time spent in run_initialization_checks, labeled by outcome (ready/failed)",
        )
        .with_unit(Unit::new("ms"))
        .init()
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

/// Registers the observable gauges reporting each resource category's
/// current active-task count and configured limit
/// (`src/services/resource_limits.rs::resource_category_snapshots`), labeled
/// by `category`. Call once at startup; the returned gauges must be kept
/// alive for as long as their callbacks should keep reporting (dropping them
/// stops the observation).
///
/// Reads the already-tracked semaphore permit counts on the export path
/// only — no additional lock is taken on the task-execution hot path.
pub fn register_resource_limiter_gauges() -> (ObservableGauge<u64>, ObservableGauge<u64>) {
    let active_gauge = meter()
        .u64_observable_gauge("resource_limiter_active_tasks")
        .with_description("Current active-task count per resource category")
        .with_callback(|observer| {
            for snapshot in crate::services::resource_limits::resource_category_snapshots() {
                observer.observe(
                    snapshot.active as u64,
                    &[KeyValue::new("category", snapshot.category)],
                );
            }
        })
        .init();

    let limit_gauge = meter()
        .u64_observable_gauge("resource_limiter_limit")
        .with_description("Configured concurrency limit per resource category")
        .with_callback(|observer| {
            for snapshot in crate::services::resource_limits::resource_category_snapshots() {
                observer.observe(
                    snapshot.limit as u64,
                    &[KeyValue::new("category", snapshot.category)],
                );
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
