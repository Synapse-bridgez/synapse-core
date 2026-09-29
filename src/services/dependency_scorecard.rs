//! Service-dependency health scorecard (issue #1334).
//!
//! Tracks the historical reliability of each external dependency — the
//! settlement network API (Stellar Horizon, `stellar::client`), Redis,
//! Postgres and Vault — as uptime, error rate and latency percentiles over
//! rolling 7/30/90-day windows. See `docs/dependency-scorecard.md`.
//!
//! # Where the data comes from
//!
//! No bespoke per-dependency instrumentation: every sample is recorded from a
//! choke point that already existed for circuit-breaking or timeouts —
//!
//! | Dependency       | Hook                                                            |
//! |------------------|-----------------------------------------------------------------|
//! | `settlement_api` | `HorizonClient`'s failsafe circuit breaker call sites            |
//! | `redis`          | `middleware::idempotency::RedisCircuitBreaker::call`             |
//! | `postgres`       | `db::queries::with_timeout`                                       |
//! | `vault`          | the `secrets::SecretsManager` refresh reads                       |
//!
//! plus circuit-breaker state transitions from the same breakers (and the
//! generic `services::circuit_breaker::CircuitBreaker`).
//!
//! # Outage vs. our own network partition
//!
//! A refused connection or a timeout looks identical whether the dependency
//! is down or *we* lost the network. The scorecard separates the two:
//!
//! - An outcome where the dependency **responded** (HTTP 5xx, a Redis error
//!   reply, a Postgres protocol error) proves it was reachable, so it is
//!   always attributed to the dependency ([`CallOutcome::DependencyFault`]).
//! - A **transport** failure (no response at all) is ambiguous. Per minute,
//!   if at least two independent dependencies saw traffic and *every* one of
//!   them was failing at the transport level at the same time, the common
//!   factor is our side, so that minute is classified
//!   [`MinuteVerdict::LocalPartition`] for the affected dependencies: its
//!   failures are reported separately (`partition_failures`) and the minute
//!   is excluded from their uptime denominator instead of counting against
//!   them. If any dependency was still reachable, the failing one is
//!   attributed its own outage.
//!
//! # Storage
//!
//! Samples accumulate per minute in-process ([`ScorecardRecorder`]); a
//! background task drains completed minutes, classifies them, folds them
//! into 5-minute [`PeriodRollup`] rows and upserts them into
//! `dependency_health_rollups`. Rows carry mergeable latency bucket counts,
//! so any window across any number of instances is aggregated exactly by
//! [`aggregate`].

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::utils::histogram::{bucket_index, bucket_quantile, merge_into};

// ---------------------------------------------------------------------------
// Vocabulary
// ---------------------------------------------------------------------------

/// The tracked external dependencies. Bounded, fixed set — safe as a metric
/// label (see docs/metrics-cardinality-convention.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dependency {
    SettlementApi,
    Redis,
    Postgres,
    Vault,
}

impl Dependency {
    pub const ALL: [Dependency; 4] = [
        Dependency::SettlementApi,
        Dependency::Redis,
        Dependency::Postgres,
        Dependency::Vault,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Dependency::SettlementApi => "settlement_api",
            Dependency::Redis => "redis",
            Dependency::Postgres => "postgres",
            Dependency::Vault => "vault",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.as_str() == s)
    }

    /// Maps a `services::circuit_breaker::CircuitBreaker` service name onto a
    /// tracked dependency, if it is one.
    pub fn from_service_name(name: &str) -> Option<Self> {
        let name = name.to_ascii_lowercase();
        if name.contains("horizon") || name.contains("stellar") || name.contains("settlement") {
            Some(Dependency::SettlementApi)
        } else if name.contains("redis") {
            Some(Dependency::Redis)
        } else if name.contains("postgres") || name == "db" || name.contains("database") {
            Some(Dependency::Postgres)
        } else if name.contains("vault") {
            Some(Dependency::Vault)
        } else {
            None
        }
    }

    fn index(self) -> usize {
        match self {
            Dependency::SettlementApi => 0,
            Dependency::Redis => 1,
            Dependency::Postgres => 2,
            Dependency::Vault => 3,
        }
    }
}

/// Outcome of one request to a dependency, from the client's perspective.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallOutcome {
    /// The dependency responded normally. Includes expected business
    /// outcomes that prove availability — a Horizon 404 for an unfunded
    /// account, a Postgres unique violation.
    Success,
    /// The dependency was reachable and answered with an error (HTTP 5xx, a
    /// Redis error reply, a Postgres protocol error). Always attributed to
    /// the dependency.
    DependencyFault,
    /// No response at all: connection refused/reset, TLS failure, timeout.
    /// Ambiguous between a dependency outage and a partition on our side;
    /// resolved per minute by [`classify_minute`].
    TransportFailure,
    /// Short-circuited by an open circuit breaker; nothing was sent.
    CircuitRejected,
}

/// A circuit-breaker state change for a dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitTransition {
    Opened,
    Closed,
}

/// Latency bucket upper bounds in ms. 12 bounds + overflow = 13 buckets per
/// row; reviewed as enough resolution for p50/p95/p99 from sub-ms Redis
/// calls to multi-second settlement API calls.
pub const LATENCY_BOUNDS_MS: [f64; 12] = [
    1.0, 2.5, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0, 5000.0,
];
pub const LATENCY_BUCKETS: usize = LATENCY_BOUNDS_MS.len() + 1;

/// Width of a persisted rollup row.
pub const ROLLUP_PERIOD_MINUTES: i64 = 5;

/// A minute counts as down when at least this fraction of its attempts
/// failed for reasons attributable to the dependency.
pub const DOWN_FAILURE_RATIO: f64 = 0.5;

/// Upper bound on buffered, not-yet-drained minutes. If the flush task dies
/// the recorder drops the oldest minutes instead of growing without bound.
const MAX_BUFFERED_MINUTES: usize = 180;

// ---------------------------------------------------------------------------
// Per-minute recording
// ---------------------------------------------------------------------------

/// Raw counts for one dependency during one wall-clock minute.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MinuteStats {
    /// All attempts, including circuit-rejected ones.
    pub calls: u64,
    pub successes: u64,
    pub dependency_faults: u64,
    pub transport_failures: u64,
    pub circuit_rejected: u64,
    pub latency_buckets: [u64; LATENCY_BUCKETS],
    /// The breaker was open at some point during this minute.
    pub circuit_open: bool,
    /// Closed→open transitions that happened during this minute.
    pub circuit_opened: u64,
}

impl MinuteStats {
    fn record(&mut self, outcome: CallOutcome, latency: Duration) {
        self.calls += 1;
        match outcome {
            CallOutcome::Success => self.successes += 1,
            CallOutcome::DependencyFault => self.dependency_faults += 1,
            CallOutcome::TransportFailure => self.transport_failures += 1,
            CallOutcome::CircuitRejected => {
                self.circuit_rejected += 1;
                self.circuit_open = true;
                // No request was sent — its "latency" says nothing about the
                // dependency.
                return;
            }
        }
        let ms = latency.as_secs_f64() * 1000.0;
        self.latency_buckets[bucket_index(&LATENCY_BOUNDS_MS, ms)] += 1;
    }

    /// Attempts that actually reached the network.
    fn sent(&self) -> u64 {
        self.calls - self.circuit_rejected
    }

    fn transport_down(&self) -> bool {
        let sent = self.sent();
        sent > 0 && self.transport_failures as f64 >= sent as f64 * DOWN_FAILURE_RATIO
    }
}

pub type MinuteRow = [MinuteStats; 4];

#[derive(Default)]
struct RecorderInner {
    minutes: BTreeMap<i64, MinuteRow>,
    /// Minute at which each dependency's breaker last opened, while open.
    open_since: [Option<i64>; 4],
}

impl RecorderInner {
    fn row(&mut self, minute: i64) -> &mut MinuteRow {
        if !self.minutes.contains_key(&minute) && self.minutes.len() >= MAX_BUFFERED_MINUTES {
            if let Some(oldest) = self.minutes.keys().next().copied() {
                self.minutes.remove(&oldest);
                tracing::warn!(
                    dropped_minute = oldest,
                    "dependency_scorecard: buffer full (flush task not draining?) — \
                     dropping oldest buffered minute"
                );
            }
        }
        self.minutes.entry(minute).or_default()
    }

    fn mark_open(&mut self, dep: Dependency, from: i64, to_inclusive: i64) {
        for m in from..=to_inclusive {
            self.row(m)[dep.index()].circuit_open = true;
        }
    }
}

/// Process-wide, in-memory accumulator for the current (not yet flushed)
/// minutes. Recording is a short mutex-guarded counter update.
#[derive(Default)]
pub struct ScorecardRecorder {
    inner: Mutex<RecorderInner>,
}

fn minute_of(unix_secs: i64) -> i64 {
    unix_secs.div_euclid(60)
}

impl ScorecardRecorder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_call_at(
        &self,
        dep: Dependency,
        outcome: CallOutcome,
        latency: Duration,
        at_unix_secs: i64,
    ) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.row(minute_of(at_unix_secs))[dep.index()].record(outcome, latency);
    }

    pub fn record_transition_at(
        &self,
        dep: Dependency,
        transition: CircuitTransition,
        at_unix_secs: i64,
    ) {
        let minute = minute_of(at_unix_secs);
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        match transition {
            CircuitTransition::Opened => {
                if inner.open_since[dep.index()].is_none() {
                    inner.open_since[dep.index()] = Some(minute);
                }
                let stats = &mut inner.row(minute)[dep.index()];
                stats.circuit_open = true;
                stats.circuit_opened += 1;
            }
            CircuitTransition::Closed => {
                if let Some(since) = inner.open_since[dep.index()].take() {
                    inner.mark_open(dep, since, minute);
                }
            }
        }
    }

    /// Removes and returns every fully-elapsed minute (strictly before the
    /// minute containing `now_unix_secs`), oldest first. Breakers that are
    /// still open mark every drained minute since they opened — including
    /// minutes with no traffic at all, which would otherwise be invisible.
    pub fn drain_completed(&self, now_unix_secs: i64) -> Vec<(i64, MinuteRow)> {
        let current = minute_of(now_unix_secs);
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        for dep in Dependency::ALL {
            if let Some(since) = inner.open_since[dep.index()] {
                if since < current {
                    inner.mark_open(dep, since, current - 1);
                    // Keep tracking from the current minute onwards.
                    inner.open_since[dep.index()] = Some(current);
                }
            }
        }
        let keep = inner.minutes.split_off(&current);
        let drained = std::mem::replace(&mut inner.minutes, keep);
        drained.into_iter().collect()
    }
}

fn recorder() -> &'static ScorecardRecorder {
    static RECORDER: OnceLock<ScorecardRecorder> = OnceLock::new();
    RECORDER.get_or_init(ScorecardRecorder::new)
}

/// Records one dependency call against the process-wide recorder.
pub fn record_call(dep: Dependency, outcome: CallOutcome, latency: Duration) {
    recorder().record_call_at(dep, outcome, latency, Utc::now().timestamp());
}

/// Records a circuit-breaker state change against the process-wide recorder.
pub fn record_circuit_transition(dep: Dependency, transition: CircuitTransition) {
    tracing::info!(
        dependency = dep.as_str(),
        transition = ?transition,
        "dependency_scorecard: circuit breaker transition"
    );
    recorder().record_transition_at(dep, transition, Utc::now().timestamp());
}

/// Records a transition if the breaker's "permits calls" state changed
/// across a call — used for failsafe-based breakers, which expose no
/// transition callbacks.
pub fn record_permitted_change(dep: Dependency, permitted_before: bool, permitted_after: bool) {
    match (permitted_before, permitted_after) {
        (true, false) => record_circuit_transition(dep, CircuitTransition::Opened),
        (false, true) => record_circuit_transition(dep, CircuitTransition::Closed),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Outcome classification for each client library
// ---------------------------------------------------------------------------

/// Classifies a Redis client error.
pub fn classify_redis_error(e: &redis::RedisError) -> CallOutcome {
    if e.is_io_error() || e.is_timeout() || e.is_connection_refusal() || e.is_connection_dropped() {
        return CallOutcome::TransportFailure;
    }
    match e.kind() {
        // Raised by our own (de)serialization / validation code paths — the
        // server answered fine.
        redis::ErrorKind::TypeError
        | redis::ErrorKind::ClientError
        | redis::ErrorKind::InvalidClientConfig => CallOutcome::Success,
        _ => CallOutcome::DependencyFault,
    }
}

/// Classifies a sqlx error.
pub fn classify_sqlx_error(e: &sqlx::Error) -> CallOutcome {
    match e {
        // Postgres answered: constraint violations, not-found, decode issues
        // on our side are not availability problems.
        sqlx::Error::Database(_)
        | sqlx::Error::RowNotFound
        | sqlx::Error::TypeNotFound { .. }
        | sqlx::Error::ColumnIndexOutOfBounds { .. }
        | sqlx::Error::ColumnNotFound(_)
        | sqlx::Error::ColumnDecode { .. }
        | sqlx::Error::Decode(_) => CallOutcome::Success,
        sqlx::Error::Io(_)
        | sqlx::Error::Tls(_)
        | sqlx::Error::PoolTimedOut
        | sqlx::Error::PoolClosed
        | sqlx::Error::WorkerCrashed => CallOutcome::TransportFailure,
        _ => CallOutcome::DependencyFault,
    }
}

/// Classifies a reqwest error from a settlement API call.
pub fn classify_reqwest_error(e: &reqwest::Error) -> CallOutcome {
    if e.is_connect() || e.is_timeout() || e.is_request() {
        CallOutcome::TransportFailure
    } else if let Some(status) = e.status() {
        if status.is_server_error() {
            CallOutcome::DependencyFault
        } else {
            CallOutcome::Success
        }
    } else if e.is_decode() || e.is_body() {
        CallOutcome::DependencyFault
    } else {
        CallOutcome::TransportFailure
    }
}

/// Classifies an HTTP status returned by a dependency.
pub fn classify_http_status(status: u16) -> CallOutcome {
    if status >= 500 {
        CallOutcome::DependencyFault
    } else {
        CallOutcome::Success
    }
}

// ---------------------------------------------------------------------------
// Minute classification (outage vs. local partition)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MinuteVerdict {
    /// No traffic and breaker closed: no evidence either way.
    Unobserved,
    Up,
    Down,
    /// Failures in this minute are attributed to our own network, not the
    /// dependency.
    LocalPartition,
}

/// Classifies one minute for every dependency at once — the partition
/// decision needs to see all dependencies together.
pub fn classify_minute(row: &MinuteRow) -> [MinuteVerdict; 4] {
    let active: Vec<&MinuteStats> = row.iter().filter(|s| s.sent() > 0).collect();
    let local_partition = active.len() >= 2 && active.iter().all(|s| s.transport_down());

    let mut verdicts = [MinuteVerdict::Unobserved; 4];
    for (i, s) in row.iter().enumerate() {
        verdicts[i] = if s.calls == 0 && !s.circuit_open {
            MinuteVerdict::Unobserved
        } else if local_partition && (s.transport_down() || (s.sent() == 0 && s.calls > 0)) {
            MinuteVerdict::LocalPartition
        } else if s.calls == 0 {
            // Breaker held open all minute with no attempts at all.
            MinuteVerdict::Down
        } else {
            let failed = s.dependency_faults + s.transport_failures + s.circuit_rejected;
            if failed as f64 >= s.calls as f64 * DOWN_FAILURE_RATIO {
                MinuteVerdict::Down
            } else {
                MinuteVerdict::Up
            }
        };
    }
    verdicts
}

// ---------------------------------------------------------------------------
// Rollups
// ---------------------------------------------------------------------------

/// One persisted row: a dependency's classified activity over one
/// [`ROLLUP_PERIOD_MINUTES`] period on one instance. All counters are
/// additive, so rows merge across periods and instances by summation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PeriodRollup {
    pub dependency: Dependency,
    pub period_start: DateTime<Utc>,
    pub instance_id: String,
    pub calls: u64,
    pub successes: u64,
    /// Attributed to the dependency: it answered with an error.
    pub dependency_faults: u64,
    /// Attributed to the dependency: no response while others were reachable.
    pub transport_failures: u64,
    /// Attributed to the dependency: rejected by an open breaker.
    pub circuit_rejected: u64,
    /// Not attributed to the dependency: failures during minutes classified
    /// as a partition on our side.
    pub partition_failures: u64,
    pub minutes_observed: u64,
    pub minutes_up: u64,
    pub minutes_partitioned: u64,
    pub circuit_open_transitions: u64,
    pub latency_buckets: Vec<u64>,
}

impl PeriodRollup {
    fn empty(dependency: Dependency, period_start: DateTime<Utc>, instance_id: &str) -> Self {
        Self {
            dependency,
            period_start,
            instance_id: instance_id.to_string(),
            calls: 0,
            successes: 0,
            dependency_faults: 0,
            transport_failures: 0,
            circuit_rejected: 0,
            partition_failures: 0,
            minutes_observed: 0,
            minutes_up: 0,
            minutes_partitioned: 0,
            circuit_open_transitions: 0,
            latency_buckets: vec![0; LATENCY_BUCKETS],
        }
    }

    /// Failures attributed to the dependency itself.
    pub fn attributed_failures(&self) -> u64 {
        self.dependency_faults + self.transport_failures + self.circuit_rejected
    }
}

fn period_start_of(minute: i64) -> DateTime<Utc> {
    let start_minute = minute - minute.rem_euclid(ROLLUP_PERIOD_MINUTES);
    Utc.timestamp_opt(start_minute * 60, 0)
        .single()
        .unwrap_or_else(Utc::now)
}

/// Classifies drained minutes and folds them into per-period rollups. Minutes
/// with no activity for a dependency produce no row for it.
pub fn rollup(minutes: &[(i64, MinuteRow)], instance_id: &str) -> Vec<PeriodRollup> {
    let mut out: BTreeMap<(DateTime<Utc>, Dependency), PeriodRollup> = BTreeMap::new();
    for (minute, row) in minutes {
        let verdicts = classify_minute(row);
        let period = period_start_of(*minute);
        for dep in Dependency::ALL {
            let s = &row[dep.index()];
            let verdict = verdicts[dep.index()];
            if verdict == MinuteVerdict::Unobserved && s.calls == 0 {
                continue;
            }
            let r = out
                .entry((period, dep))
                .or_insert_with(|| PeriodRollup::empty(dep, period, instance_id));
            r.calls += s.calls;
            r.successes += s.successes;
            r.dependency_faults += s.dependency_faults;
            r.circuit_open_transitions += s.circuit_opened;
            merge_into(&mut r.latency_buckets, &s.latency_buckets);
            match verdict {
                MinuteVerdict::LocalPartition => {
                    r.partition_failures += s.transport_failures + s.circuit_rejected;
                    r.minutes_partitioned += 1;
                }
                MinuteVerdict::Up | MinuteVerdict::Down => {
                    r.transport_failures += s.transport_failures;
                    r.circuit_rejected += s.circuit_rejected;
                    r.minutes_observed += 1;
                    if verdict == MinuteVerdict::Up {
                        r.minutes_up += 1;
                    }
                }
                MinuteVerdict::Unobserved => {
                    r.transport_failures += s.transport_failures;
                    r.circuit_rejected += s.circuit_rejected;
                }
            }
        }
    }
    out.into_values().collect()
}

// ---------------------------------------------------------------------------
// Aggregation into the scorecard
// ---------------------------------------------------------------------------

/// Default reporting windows, in days.
pub const DEFAULT_WINDOWS_DAYS: [u32; 3] = [7, 30, 90];
/// Longest window the API will aggregate (retention is a little longer).
pub const MAX_WINDOW_DAYS: u32 = 90;
/// Rows older than this are pruned.
pub const RETENTION_DAYS: i64 = 100;

/// Below this fraction of the window having any data, no grade is given.
pub const MIN_COVERAGE_FOR_GRADE: f64 = 0.5;
/// Below this many calls in the window, no grade is given.
pub const MIN_CALLS_FOR_GRADE: u64 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Grade {
    /// ≥ 99.9% uptime and ≤ 1% attributed error rate.
    Healthy,
    /// ≥ 99% uptime and ≤ 5% attributed error rate.
    Watch,
    /// Worse than `Watch`: a candidate for resilience investment.
    NeedsInvestment,
    /// Not enough data in the window to judge.
    InsufficientData,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScorecardEntry {
    pub dependency: Dependency,
    pub window_days: u32,
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    pub calls: u64,
    pub successes: u64,
    pub attributed_failures: u64,
    pub partition_failures: u64,
    /// Fraction of observed instance-minutes the dependency was up.
    pub uptime_ratio: Option<f64>,
    /// Attributed failures / calls.
    pub error_rate: Option<f64>,
    /// Failures attributed to our own network / calls. Reported separately
    /// so it never inflates the dependency's error rate.
    pub partition_error_rate: Option<f64>,
    pub latency_p50_ms: Option<f64>,
    pub latency_p95_ms: Option<f64>,
    pub latency_p99_ms: Option<f64>,
    pub minutes_observed: u64,
    pub minutes_up: u64,
    pub minutes_partitioned: u64,
    pub circuit_open_transitions: u64,
    /// Fraction of the window's rollup periods that have any data for this
    /// dependency (on any instance).
    pub coverage_ratio: f64,
    pub grade: Grade,
}

fn ratio(num: u64, den: u64) -> Option<f64> {
    (den > 0).then(|| num as f64 / den as f64)
}

pub fn grade(uptime: Option<f64>, error_rate: Option<f64>, calls: u64, coverage: f64) -> Grade {
    let (Some(uptime), Some(error_rate)) = (uptime, error_rate) else {
        return Grade::InsufficientData;
    };
    if calls < MIN_CALLS_FOR_GRADE || coverage < MIN_COVERAGE_FOR_GRADE {
        Grade::InsufficientData
    } else if uptime >= 0.999 && error_rate <= 0.01 {
        Grade::Healthy
    } else if uptime >= 0.99 && error_rate <= 0.05 {
        Grade::Watch
    } else {
        Grade::NeedsInvestment
    }
}

/// Aggregates `rollups` for `dep` over `[window_end - window_days, window_end)`.
pub fn aggregate(
    rollups: &[PeriodRollup],
    dep: Dependency,
    window_end: DateTime<Utc>,
    window_days: u32,
) -> ScorecardEntry {
    let window_start = window_end - chrono::Duration::days(window_days as i64);
    let mut calls = 0;
    let mut successes = 0;
    let mut attributed = 0;
    let mut partition = 0;
    let mut minutes_observed = 0;
    let mut minutes_up = 0;
    let mut minutes_partitioned = 0;
    let mut transitions = 0;
    let mut buckets = vec![0u64; LATENCY_BUCKETS];
    let mut periods = std::collections::HashSet::new();

    for r in rollups.iter().filter(|r| {
        r.dependency == dep && r.period_start >= window_start && r.period_start < window_end
    }) {
        calls += r.calls;
        successes += r.successes;
        attributed += r.attributed_failures();
        partition += r.partition_failures;
        minutes_observed += r.minutes_observed;
        minutes_up += r.minutes_up;
        minutes_partitioned += r.minutes_partitioned;
        transitions += r.circuit_open_transitions;
        merge_into(&mut buckets, &r.latency_buckets);
        periods.insert(r.period_start);
    }

    let total_periods = (window_days as i64 * 24 * 60 / ROLLUP_PERIOD_MINUTES).max(1);
    let coverage_ratio = (periods.len() as f64 / total_periods as f64).min(1.0);
    let uptime_ratio = ratio(minutes_up, minutes_observed);
    let error_rate = ratio(attributed, calls);

    ScorecardEntry {
        dependency: dep,
        window_days,
        window_start,
        window_end,
        calls,
        successes,
        attributed_failures: attributed,
        partition_failures: partition,
        uptime_ratio,
        error_rate,
        partition_error_rate: ratio(partition, calls),
        latency_p50_ms: bucket_quantile(&LATENCY_BOUNDS_MS, &buckets, 0.50),
        latency_p95_ms: bucket_quantile(&LATENCY_BOUNDS_MS, &buckets, 0.95),
        latency_p99_ms: bucket_quantile(&LATENCY_BOUNDS_MS, &buckets, 0.99),
        minutes_observed,
        minutes_up,
        minutes_partitioned,
        circuit_open_transitions: transitions,
        coverage_ratio,
        grade: grade(uptime_ratio, error_rate, calls, coverage_ratio),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Scorecard {
    pub generated_at: DateTime<Utc>,
    pub windows_days: Vec<u32>,
    pub entries: Vec<ScorecardEntry>,
}

/// Builds a full scorecard: one entry per dependency per window.
pub fn build_scorecard(
    rollups: &[PeriodRollup],
    window_end: DateTime<Utc>,
    windows_days: &[u32],
) -> Scorecard {
    let mut entries = Vec::new();
    for dep in Dependency::ALL {
        for &w in windows_days {
            entries.push(aggregate(rollups, dep, window_end, w));
        }
    }
    Scorecard {
        generated_at: window_end,
        windows_days: windows_days.to_vec(),
        entries,
    }
}

/// Parses a `windows=7,30,90` query value, clamped to `1..=MAX_WINDOW_DAYS`.
pub fn parse_windows(raw: Option<&str>) -> Result<Vec<u32>, String> {
    let Some(raw) = raw.filter(|s| !s.trim().is_empty()) else {
        return Ok(DEFAULT_WINDOWS_DAYS.to_vec());
    };
    let mut out = Vec::new();
    for part in raw.split(',') {
        let days: u32 = part
            .trim()
            .parse()
            .map_err(|_| format!("invalid window '{part}': expected whole days"))?;
        if days == 0 || days > MAX_WINDOW_DAYS {
            return Err(format!("window {days} out of range 1..={MAX_WINDOW_DAYS}"));
        }
        if !out.contains(&days) {
            out.push(days);
        }
    }
    if out.len() > 6 {
        return Err("at most 6 windows per request".to_string());
    }
    Ok(out)
}

/// CSV export of raw rollup rows for capacity-planning / postmortem tooling.
pub fn rollups_to_csv(rows: &[PeriodRollup]) -> String {
    let mut out = String::from(
        "dependency,period_start,instance_id,calls,successes,dependency_faults,\
         transport_failures,circuit_rejected,partition_failures,minutes_observed,minutes_up,\
         minutes_partitioned,circuit_open_transitions,latency_buckets\n",
    );
    for r in rows {
        let buckets = r
            .latency_buckets
            .iter()
            .map(|b| b.to_string())
            .collect::<Vec<_>>()
            .join(";");
        // instance_id is operator-controlled (INSTANCE_ID/HOSTNAME); strip
        // separators rather than quote.
        let instance = r.instance_id.replace([',', '\n', '\r', '"'], "_");
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
            r.dependency.as_str(),
            r.period_start.to_rfc3339(),
            instance,
            r.calls,
            r.successes,
            r.dependency_faults,
            r.transport_failures,
            r.circuit_rejected,
            r.partition_failures,
            r.minutes_observed,
            r.minutes_up,
            r.minutes_partitioned,
            r.circuit_open_transitions,
            buckets
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// Upserts rollups. A period flushed in several parts (the flush runs every
/// minute) is summed into one row, latency buckets element-wise.
pub async fn persist_rollups(
    pool: &sqlx::PgPool,
    rows: &[PeriodRollup],
) -> Result<(), sqlx::Error> {
    for r in rows {
        let buckets: Vec<i64> = r.latency_buckets.iter().map(|&b| to_i64(b)).collect();
        sqlx::query(
            r#"
            INSERT INTO dependency_health_rollups (
                dependency, period_start, instance_id, calls, successes,
                dependency_faults, transport_failures, circuit_rejected,
                partition_failures, minutes_observed, minutes_up,
                minutes_partitioned, circuit_open_transitions, latency_buckets
            ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)
            ON CONFLICT (dependency, period_start, instance_id) DO UPDATE SET
                calls = dependency_health_rollups.calls + EXCLUDED.calls,
                successes = dependency_health_rollups.successes + EXCLUDED.successes,
                dependency_faults = dependency_health_rollups.dependency_faults + EXCLUDED.dependency_faults,
                transport_failures = dependency_health_rollups.transport_failures + EXCLUDED.transport_failures,
                circuit_rejected = dependency_health_rollups.circuit_rejected + EXCLUDED.circuit_rejected,
                partition_failures = dependency_health_rollups.partition_failures + EXCLUDED.partition_failures,
                minutes_observed = dependency_health_rollups.minutes_observed + EXCLUDED.minutes_observed,
                minutes_up = dependency_health_rollups.minutes_up + EXCLUDED.minutes_up,
                minutes_partitioned = dependency_health_rollups.minutes_partitioned + EXCLUDED.minutes_partitioned,
                circuit_open_transitions = dependency_health_rollups.circuit_open_transitions + EXCLUDED.circuit_open_transitions,
                latency_buckets = ARRAY(
                    SELECT COALESCE(a, 0) + COALESCE(b, 0)
                    FROM unnest(dependency_health_rollups.latency_buckets, EXCLUDED.latency_buckets) AS t(a, b)
                ),
                recorded_at = NOW()
            "#,
        )
        .bind(r.dependency.as_str())
        .bind(r.period_start)
        .bind(&r.instance_id)
        .bind(to_i64(r.calls))
        .bind(to_i64(r.successes))
        .bind(to_i64(r.dependency_faults))
        .bind(to_i64(r.transport_failures))
        .bind(to_i64(r.circuit_rejected))
        .bind(to_i64(r.partition_failures))
        .bind(to_i64(r.minutes_observed))
        .bind(to_i64(r.minutes_up))
        .bind(to_i64(r.minutes_partitioned))
        .bind(to_i64(r.circuit_open_transitions))
        .bind(&buckets)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// Loads raw rollup rows in `[since, until)`, optionally for one dependency.
pub async fn load_rollups(
    pool: &sqlx::PgPool,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
    dependency: Option<Dependency>,
) -> Result<Vec<PeriodRollup>, sqlx::Error> {
    use sqlx::Row;
    let rows = sqlx::query(
        r#"
        SELECT dependency, period_start, instance_id, calls, successes,
               dependency_faults, transport_failures, circuit_rejected,
               partition_failures, minutes_observed, minutes_up,
               minutes_partitioned, circuit_open_transitions, latency_buckets
        FROM dependency_health_rollups
        WHERE period_start >= $1 AND period_start < $2
          AND ($3::text IS NULL OR dependency = $3)
        ORDER BY period_start, dependency, instance_id
        "#,
    )
    .bind(since)
    .bind(until)
    .bind(dependency.map(|d| d.as_str()))
    .fetch_all(pool)
    .await?;

    let u = |v: i64| v.max(0) as u64;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let dep: String = row.try_get("dependency")?;
        let Some(dependency) = Dependency::parse(&dep) else {
            continue;
        };
        let buckets: Vec<i64> = row.try_get("latency_buckets")?;
        out.push(PeriodRollup {
            dependency,
            period_start: row.try_get("period_start")?,
            instance_id: row.try_get("instance_id")?,
            calls: u(row.try_get("calls")?),
            successes: u(row.try_get("successes")?),
            dependency_faults: u(row.try_get("dependency_faults")?),
            transport_failures: u(row.try_get("transport_failures")?),
            circuit_rejected: u(row.try_get("circuit_rejected")?),
            partition_failures: u(row.try_get("partition_failures")?),
            minutes_observed: u(row.try_get("minutes_observed")?),
            minutes_up: u(row.try_get("minutes_up")?),
            minutes_partitioned: u(row.try_get("minutes_partitioned")?),
            circuit_open_transitions: u(row.try_get("circuit_open_transitions")?),
            latency_buckets: buckets.into_iter().map(u).collect(),
        });
    }
    Ok(out)
}

pub async fn prune_rollups(
    pool: &sqlx::PgPool,
    older_than: DateTime<Utc>,
) -> Result<u64, sqlx::Error> {
    let res = sqlx::query("DELETE FROM dependency_health_rollups WHERE period_start < $1")
        .bind(older_than)
        .execute(pool)
        .await?;
    Ok(res.rows_affected())
}

/// Builds the scorecard straight from the database.
pub async fn load_scorecard(
    pool: &sqlx::PgPool,
    now: DateTime<Utc>,
    windows_days: &[u32],
) -> Result<Scorecard, sqlx::Error> {
    let longest = windows_days
        .iter()
        .copied()
        .max()
        .unwrap_or(MAX_WINDOW_DAYS);
    let rows = load_rollups(
        pool,
        now - chrono::Duration::days(longest as i64),
        now,
        None,
    )
    .await?;
    Ok(build_scorecard(&rows, now, windows_days))
}

// ---------------------------------------------------------------------------
// Background flush + exported gauges
// ---------------------------------------------------------------------------

/// The most recently computed scorecard, read by the exported gauges.
static LAST_SCORECARD: RwLock<Option<Scorecard>> = RwLock::new(None);

fn set_last_scorecard(s: Scorecard) {
    if let Ok(mut guard) = LAST_SCORECARD.write() {
        *guard = Some(s);
    }
}

fn last_scorecard_entries() -> Vec<ScorecardEntry> {
    LAST_SCORECARD
        .read()
        .ok()
        .and_then(|g| g.as_ref().map(|s| s.entries.clone()))
        .unwrap_or_default()
}

/// Instance identity used on persisted rows: `INSTANCE_ID`, else `HOSTNAME`,
/// else the process id.
pub fn instance_id() -> String {
    std::env::var("INSTANCE_ID")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| format!("pid-{}", std::process::id()))
}

/// Spawns the flush loop: every minute, drain completed minutes, classify,
/// roll up and persist (retrying rows that failed to persist on the next
/// tick, bounded); every hour, prune old rows and refresh the cached
/// scorecard behind the gauges.
pub fn spawn_flush_task(pool: sqlx::PgPool) -> tokio::task::JoinHandle<()> {
    let instance = instance_id();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(60));
        let mut pending: Vec<PeriodRollup> = Vec::new();
        let mut ticks: u64 = 0;
        loop {
            ticker.tick().await;
            let drained = recorder().drain_completed(Utc::now().timestamp());
            pending.extend(rollup(&drained, &instance));
            if !pending.is_empty() {
                match persist_rollups(&pool, &pending).await {
                    Ok(()) => pending.clear(),
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            pending = pending.len(),
                            "dependency_scorecard: failed to persist rollups; will retry"
                        );
                        // An upsert that partially applied before failing
                        // would be double-counted on retry; accept that
                        // (rare, bounded) over losing the data.
                        let overflow = pending.len().saturating_sub(2_000);
                        pending.drain(..overflow);
                    }
                }
            }
            if ticks % 60 == 0 {
                let now = Utc::now();
                if let Err(e) =
                    prune_rollups(&pool, now - chrono::Duration::days(RETENTION_DAYS)).await
                {
                    tracing::warn!(error = %e, "dependency_scorecard: prune failed");
                }
                match load_scorecard(&pool, now, &DEFAULT_WINDOWS_DAYS).await {
                    Ok(s) => set_last_scorecard(s),
                    Err(e) => tracing::warn!(error = %e, "dependency_scorecard: refresh failed"),
                }
            }
            ticks += 1;
        }
    })
}

/// Registers gauges exposing the cached scorecard, labeled by `dependency`
/// (4 values) and `window` (3 values) — 12 series per gauge. Keep the
/// returned handles alive for the process lifetime.
pub fn register_scorecard_gauges() -> Vec<opentelemetry::metrics::ObservableGauge<f64>> {
    use opentelemetry::KeyValue;
    let meter = opentelemetry::global::meter("synapse-core");
    type Pick = fn(&ScorecardEntry) -> Option<f64>;
    let specs: [(&'static str, &'static str, Pick); 4] = [
        (
            "dependency_scorecard_uptime_ratio",
            "Dependency uptime over the rolling window (observed minutes up / observed)",
            |e| e.uptime_ratio,
        ),
        (
            "dependency_scorecard_error_rate",
            "Dependency-attributed error rate over the rolling window",
            |e| e.error_rate,
        ),
        (
            "dependency_scorecard_partition_error_rate",
            "Error rate attributed to our own network partition over the rolling window",
            |e| e.partition_error_rate,
        ),
        (
            "dependency_scorecard_latency_p95_ms",
            "Dependency p95 latency over the rolling window, in ms",
            |e| e.latency_p95_ms,
        ),
    ];
    specs
        .into_iter()
        .map(|(name, desc, pick)| {
            meter
                .f64_observable_gauge(name)
                .with_description(desc)
                .with_callback(move |observer| {
                    for e in last_scorecard_entries() {
                        if let Some(v) = pick(&e) {
                            observer.observe(
                                v,
                                &[
                                    KeyValue::new("dependency", e.dependency.as_str()),
                                    KeyValue::new("window", format!("{}d", e.window_days)),
                                ],
                            );
                        }
                    }
                })
                .init()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const INSTANCE: &str = "test-instance";

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    fn row_with(entries: &[(Dependency, CallOutcome, u64)]) -> MinuteRow {
        let mut row: MinuteRow = Default::default();
        for &(dep, outcome, n) in entries {
            for _ in 0..n {
                row[dep.index()].record(outcome, ms(10));
            }
        }
        row
    }

    // ── Vocabulary ───────────────────────────────────────────────────────

    #[test]
    fn dependency_names_round_trip() {
        for dep in Dependency::ALL {
            assert_eq!(Dependency::parse(dep.as_str()), Some(dep));
        }
        assert_eq!(Dependency::parse("mysql"), None);
    }

    #[test]
    fn service_names_map_to_dependencies() {
        assert_eq!(
            Dependency::from_service_name("stellar-horizon"),
            Some(Dependency::SettlementApi)
        );
        assert_eq!(
            Dependency::from_service_name("Redis-cache"),
            Some(Dependency::Redis)
        );
        assert_eq!(
            Dependency::from_service_name("postgres"),
            Some(Dependency::Postgres)
        );
        assert_eq!(
            Dependency::from_service_name("vault"),
            Some(Dependency::Vault)
        );
        assert_eq!(Dependency::from_service_name("webhook-endpoint"), None);
    }

    #[test]
    fn http_status_classification() {
        assert_eq!(classify_http_status(200), CallOutcome::Success);
        assert_eq!(classify_http_status(404), CallOutcome::Success);
        assert_eq!(classify_http_status(503), CallOutcome::DependencyFault);
    }

    #[test]
    fn redis_error_classification() {
        let io = redis::RedisError::from(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "refused",
        ));
        assert_eq!(classify_redis_error(&io), CallOutcome::TransportFailure);
        let reply = redis::RedisError::from((redis::ErrorKind::ResponseError, "ERR boom"));
        assert_eq!(classify_redis_error(&reply), CallOutcome::DependencyFault);
        let ours = redis::RedisError::from((redis::ErrorKind::TypeError, "deserialization failed"));
        assert_eq!(classify_redis_error(&ours), CallOutcome::Success);
    }

    #[test]
    fn sqlx_error_classification() {
        assert_eq!(
            classify_sqlx_error(&sqlx::Error::RowNotFound),
            CallOutcome::Success
        );
        assert_eq!(
            classify_sqlx_error(&sqlx::Error::PoolTimedOut),
            CallOutcome::TransportFailure
        );
        assert_eq!(
            classify_sqlx_error(&sqlx::Error::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "reset"
            ))),
            CallOutcome::TransportFailure
        );
        assert_eq!(
            classify_sqlx_error(&sqlx::Error::Protocol("bad".into())),
            CallOutcome::DependencyFault
        );
    }

    // ── Recorder ─────────────────────────────────────────────────────────

    #[test]
    fn recorder_buckets_by_minute_and_drains_only_completed_minutes() {
        let rec = ScorecardRecorder::new();
        rec.record_call_at(Dependency::Redis, CallOutcome::Success, ms(3), 120);
        rec.record_call_at(Dependency::Redis, CallOutcome::Success, ms(3), 179);
        rec.record_call_at(Dependency::Redis, CallOutcome::Success, ms(3), 180);

        let drained = rec.drain_completed(185);
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].0, 2);
        assert_eq!(drained[0].1[Dependency::Redis.index()].calls, 2);

        // Minute 3 is still in progress; draining later picks it up once.
        assert!(rec.drain_completed(239).is_empty());
        let later = rec.drain_completed(240);
        assert_eq!(later.len(), 1);
        assert_eq!(later[0].0, 3);
    }

    #[test]
    fn rejected_calls_do_not_record_latency() {
        let mut s = MinuteStats::default();
        s.record(CallOutcome::CircuitRejected, ms(5000));
        assert_eq!(s.latency_buckets.iter().sum::<u64>(), 0);
        assert!(s.circuit_open);
        s.record(CallOutcome::Success, ms(7));
        assert_eq!(s.latency_buckets[bucket_index(&LATENCY_BOUNDS_MS, 7.0)], 1);
    }

    #[test]
    fn open_breaker_marks_idle_minutes_until_closed() {
        let rec = ScorecardRecorder::new();
        rec.record_transition_at(Dependency::SettlementApi, CircuitTransition::Opened, 60);
        // Minutes 1..=3 elapse with no traffic at all.
        let drained = rec.drain_completed(4 * 60);
        let open: Vec<i64> = drained
            .iter()
            .filter(|(_, r)| r[Dependency::SettlementApi.index()].circuit_open)
            .map(|(m, _)| *m)
            .collect();
        assert_eq!(open, vec![1, 2, 3]);
        assert_eq!(
            drained[0].1[Dependency::SettlementApi.index()].circuit_opened,
            1
        );

        rec.record_transition_at(
            Dependency::SettlementApi,
            CircuitTransition::Closed,
            5 * 60 + 5,
        );
        let drained = rec.drain_completed(7 * 60);
        let open: Vec<i64> = drained
            .iter()
            .filter(|(_, r)| r[Dependency::SettlementApi.index()].circuit_open)
            .map(|(m, _)| *m)
            .collect();
        assert_eq!(open, vec![4, 5]);
        // Closed: nothing further is marked.
        assert!(rec.drain_completed(9 * 60).is_empty());
    }

    #[test]
    fn recorder_buffer_is_bounded() {
        let rec = ScorecardRecorder::new();
        for m in 0..(MAX_BUFFERED_MINUTES as i64 + 20) {
            rec.record_call_at(Dependency::Postgres, CallOutcome::Success, ms(1), m * 60);
        }
        let drained = rec.drain_completed(i64::MAX / 2);
        assert_eq!(drained.len(), MAX_BUFFERED_MINUTES);
        assert_eq!(drained[0].0, 20, "oldest minutes are the ones dropped");
    }

    #[test]
    fn permitted_change_detection() {
        // Only asserts it does not panic and ignores no-op changes; the
        // transition itself is covered by the recorder tests.
        record_permitted_change(Dependency::Vault, true, true);
        record_permitted_change(Dependency::Vault, false, false);
    }

    // ── Outage vs. local partition ───────────────────────────────────────

    #[test]
    fn single_dependency_transport_failure_is_its_outage() {
        let row = row_with(&[
            (Dependency::Redis, CallOutcome::TransportFailure, 10),
            (Dependency::Postgres, CallOutcome::Success, 50),
        ]);
        let v = classify_minute(&row);
        assert_eq!(v[Dependency::Redis.index()], MinuteVerdict::Down);
        assert_eq!(v[Dependency::Postgres.index()], MinuteVerdict::Up);
        assert_eq!(v[Dependency::Vault.index()], MinuteVerdict::Unobserved);
    }

    #[test]
    fn all_dependencies_unreachable_is_a_local_partition() {
        let row = row_with(&[
            (Dependency::Redis, CallOutcome::TransportFailure, 10),
            (Dependency::Postgres, CallOutcome::TransportFailure, 30),
            (Dependency::SettlementApi, CallOutcome::TransportFailure, 3),
        ]);
        let v = classify_minute(&row);
        for dep in [
            Dependency::Redis,
            Dependency::Postgres,
            Dependency::SettlementApi,
        ] {
            assert_eq!(v[dep.index()], MinuteVerdict::LocalPartition, "{dep:?}");
        }
    }

    #[test]
    fn a_responding_dependency_rules_out_partition() {
        // Redis answers with error replies — so our network is fine — while
        // Postgres is unreachable: that's Postgres' outage.
        let row = row_with(&[
            (Dependency::Redis, CallOutcome::DependencyFault, 10),
            (Dependency::Postgres, CallOutcome::TransportFailure, 30),
        ]);
        let v = classify_minute(&row);
        assert_eq!(v[Dependency::Redis.index()], MinuteVerdict::Down);
        assert_eq!(v[Dependency::Postgres.index()], MinuteVerdict::Down);
    }

    #[test]
    fn a_single_active_dependency_cannot_be_called_a_partition() {
        let row = row_with(&[(Dependency::SettlementApi, CallOutcome::TransportFailure, 5)]);
        assert_eq!(
            classify_minute(&row)[Dependency::SettlementApi.index()],
            MinuteVerdict::Down
        );
    }

    #[test]
    fn circuit_rejected_only_dependency_follows_partition_verdict() {
        let mut row = row_with(&[
            (Dependency::Redis, CallOutcome::TransportFailure, 5),
            (Dependency::Postgres, CallOutcome::TransportFailure, 5),
            (Dependency::SettlementApi, CallOutcome::CircuitRejected, 4),
        ]);
        let v = classify_minute(&row);
        assert_eq!(
            v[Dependency::SettlementApi.index()],
            MinuteVerdict::LocalPartition
        );

        // Without a partition, a breaker held open all minute is downtime.
        row[Dependency::Postgres.index()] = MinuteStats::default();
        row[Dependency::Postgres.index()].record(CallOutcome::Success, ms(2));
        let v = classify_minute(&row);
        assert_eq!(v[Dependency::SettlementApi.index()], MinuteVerdict::Down);
    }

    #[test]
    fn open_breaker_with_no_calls_is_down() {
        let mut row: MinuteRow = Default::default();
        row[Dependency::Vault.index()].circuit_open = true;
        assert_eq!(
            classify_minute(&row)[Dependency::Vault.index()],
            MinuteVerdict::Down
        );
    }

    #[test]
    fn minority_failures_keep_minute_up() {
        let row = row_with(&[
            (Dependency::Postgres, CallOutcome::Success, 9),
            (Dependency::Postgres, CallOutcome::TransportFailure, 1),
        ]);
        assert_eq!(
            classify_minute(&row)[Dependency::Postgres.index()],
            MinuteVerdict::Up
        );
    }

    // ── Rollups ──────────────────────────────────────────────────────────

    #[test]
    fn rollup_groups_into_five_minute_periods_and_separates_partition_failures() {
        let healthy = row_with(&[
            (Dependency::Redis, CallOutcome::Success, 10),
            (Dependency::Postgres, CallOutcome::Success, 10),
        ]);
        let partition = row_with(&[
            (Dependency::Redis, CallOutcome::TransportFailure, 4),
            (Dependency::Postgres, CallOutcome::TransportFailure, 6),
        ]);
        let redis_outage = row_with(&[
            (Dependency::Redis, CallOutcome::TransportFailure, 8),
            (Dependency::Postgres, CallOutcome::Success, 10),
        ]);
        // Minutes 0,1,2 in period 0; minute 5 in period 1.
        let minutes = vec![
            (0, healthy.clone()),
            (1, partition),
            (2, redis_outage),
            (5, healthy),
        ];
        let rows = rollup(&minutes, INSTANCE);

        let redis_p0 = rows
            .iter()
            .find(|r| r.dependency == Dependency::Redis && r.period_start.timestamp() == 0)
            .unwrap();
        assert_eq!(redis_p0.calls, 22);
        assert_eq!(redis_p0.successes, 10);
        assert_eq!(redis_p0.partition_failures, 4);
        assert_eq!(redis_p0.transport_failures, 8);
        assert_eq!(redis_p0.minutes_observed, 2);
        assert_eq!(redis_p0.minutes_up, 1);
        assert_eq!(redis_p0.minutes_partitioned, 1);
        assert_eq!(redis_p0.latency_buckets.iter().sum::<u64>(), 22);
        assert_eq!(redis_p0.instance_id, INSTANCE);

        let redis_p1 = rows
            .iter()
            .find(|r| r.dependency == Dependency::Redis && r.period_start.timestamp() == 300)
            .unwrap();
        assert_eq!(redis_p1.minutes_up, 1);

        // Vault had no activity: no rows at all.
        assert!(rows.iter().all(|r| r.dependency != Dependency::Vault));
    }

    // ── Aggregation against fixture history ──────────────────────────────

    /// Synthesises 90 days of per-period rows for one dependency with known
    /// characteristics.
    fn fixture(
        dep: Dependency,
        end: DateTime<Utc>,
        days: i64,
        down_every: Option<i64>,
        latency_bucket: usize,
    ) -> Vec<PeriodRollup> {
        let periods = days * 24 * 60 / ROLLUP_PERIOD_MINUTES;
        let mut rows = Vec::new();
        for p in 0..periods {
            let start = end - chrono::Duration::minutes((p + 1) * ROLLUP_PERIOD_MINUTES);
            let mut r = PeriodRollup::empty(dep, start, INSTANCE);
            r.calls = 100;
            r.minutes_observed = ROLLUP_PERIOD_MINUTES as u64;
            let down = down_every.map(|n| p % n == 0).unwrap_or(false);
            if down {
                r.successes = 0;
                r.transport_failures = 100;
                r.minutes_up = 0;
            } else {
                r.successes = 100;
                r.minutes_up = ROLLUP_PERIOD_MINUTES as u64;
            }
            r.latency_buckets[latency_bucket] = r.successes + r.transport_failures;
            rows.push(r);
        }
        rows
    }

    fn end() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap()
    }

    #[test]
    fn perfect_dependency_grades_healthy_across_windows() {
        let rows = fixture(Dependency::Postgres, end(), 90, None, 3);
        let card = build_scorecard(&rows, end(), &DEFAULT_WINDOWS_DAYS);
        for e in card
            .entries
            .iter()
            .filter(|e| e.dependency == Dependency::Postgres)
        {
            assert_eq!(e.uptime_ratio, Some(1.0));
            assert_eq!(e.error_rate, Some(0.0));
            assert!(
                (e.coverage_ratio - 1.0).abs() < 1e-9,
                "{}",
                e.coverage_ratio
            );
            assert_eq!(e.grade, Grade::Healthy);
            // All samples in bucket 3: (5, 10] ms.
            let p95 = e.latency_p95_ms.unwrap();
            assert!(p95 > 5.0 && p95 <= 10.0, "p95 {p95}");
        }
        assert_eq!(
            card.entries
                .iter()
                .filter(|e| e.dependency == Dependency::Postgres)
                .map(|e| e.window_days)
                .collect::<Vec<_>>(),
            vec![7, 30, 90]
        );
    }

    #[test]
    fn known_failure_rate_is_recovered_exactly() {
        // One period in 50 is a full outage: 98% uptime, 2% error rate.
        let rows = fixture(Dependency::SettlementApi, end(), 90, Some(50), 8);
        let e = aggregate(&rows, Dependency::SettlementApi, end(), 90);
        let uptime = e.uptime_ratio.unwrap();
        let err = e.error_rate.unwrap();
        assert!((uptime - 0.98).abs() < 0.001, "uptime {uptime}");
        assert!((err - 0.02).abs() < 0.001, "error rate {err}");
        assert_eq!(e.grade, Grade::NeedsInvestment);
    }

    #[test]
    fn watch_grade_between_thresholds() {
        // One period in 200 down: 99.5% uptime, 0.5% errors.
        let rows = fixture(Dependency::Redis, end(), 30, Some(200), 1);
        let e = aggregate(&rows, Dependency::Redis, end(), 30);
        assert_eq!(e.grade, Grade::Watch);
    }

    #[test]
    fn windows_only_include_their_own_history() {
        // A 7-day-old-and-older outage history, clean last week.
        let mut rows = fixture(Dependency::Vault, end(), 7, None, 5);
        let older_end = end() - chrono::Duration::days(7);
        rows.extend(fixture(Dependency::Vault, older_end, 83, Some(2), 5));
        let week = aggregate(&rows, Dependency::Vault, end(), 7);
        let quarter = aggregate(&rows, Dependency::Vault, end(), 90);
        assert_eq!(week.uptime_ratio, Some(1.0));
        assert!(quarter.uptime_ratio.unwrap() < 0.6);
    }

    #[test]
    fn partition_minutes_do_not_count_against_the_dependency() {
        let mut r = PeriodRollup::empty(
            Dependency::Redis,
            end() - chrono::Duration::hours(1),
            INSTANCE,
        );
        r.calls = 500;
        r.successes = 400;
        r.partition_failures = 100;
        r.minutes_observed = 4;
        r.minutes_up = 4;
        r.minutes_partitioned = 1;
        r.latency_buckets[0] = 500;
        let e = aggregate(&[r], Dependency::Redis, end(), 7);
        assert_eq!(e.uptime_ratio, Some(1.0));
        assert_eq!(e.error_rate, Some(0.0));
        assert_eq!(e.partition_error_rate, Some(0.2));
        assert_eq!(e.minutes_partitioned, 1);
    }

    #[test]
    fn multiple_instances_merge_by_summation() {
        let start = end() - chrono::Duration::hours(2);
        let mut a = PeriodRollup::empty(Dependency::Postgres, start, "a");
        a.calls = 100;
        a.successes = 100;
        a.minutes_observed = 5;
        a.minutes_up = 5;
        a.latency_buckets[2] = 100;
        let mut b = PeriodRollup::empty(Dependency::Postgres, start, "b");
        b.calls = 100;
        b.dependency_faults = 100;
        b.minutes_observed = 5;
        b.latency_buckets[10] = 100;
        let e = aggregate(&[a, b], Dependency::Postgres, end(), 7);
        assert_eq!(e.calls, 200);
        assert_eq!(e.uptime_ratio, Some(0.5));
        assert_eq!(e.error_rate, Some(0.5));
        // p50 sits at the edge between the two instances' buckets; p99 in b's.
        assert!(e.latency_p99_ms.unwrap() > LATENCY_BOUNDS_MS[9]);
    }

    #[test]
    fn empty_history_is_insufficient_data() {
        let e = aggregate(&[], Dependency::Vault, end(), 30);
        assert_eq!(e.calls, 0);
        assert_eq!(e.uptime_ratio, None);
        assert_eq!(e.latency_p50_ms, None);
        assert_eq!(e.grade, Grade::InsufficientData);
        assert_eq!(e.coverage_ratio, 0.0);
    }

    #[test]
    fn sparse_history_is_insufficient_data_even_if_perfect() {
        // One day of data in a 90-day window: coverage ~1.1%.
        let rows = fixture(Dependency::Redis, end(), 1, None, 1);
        let e = aggregate(&rows, Dependency::Redis, end(), 90);
        assert_eq!(e.uptime_ratio, Some(1.0));
        assert_eq!(e.grade, Grade::InsufficientData);
    }

    #[test]
    fn grade_requires_minimum_calls() {
        assert_eq!(
            grade(Some(1.0), Some(0.0), 10, 1.0),
            Grade::InsufficientData
        );
        assert_eq!(grade(Some(1.0), Some(0.0), 1000, 1.0), Grade::Healthy);
        assert_eq!(grade(None, Some(0.0), 1000, 1.0), Grade::InsufficientData);
    }

    // ── Query parsing / export ───────────────────────────────────────────

    #[test]
    fn parse_windows_defaults_validates_and_dedupes() {
        assert_eq!(parse_windows(None).unwrap(), vec![7, 30, 90]);
        assert_eq!(parse_windows(Some("  ")).unwrap(), vec![7, 30, 90]);
        assert_eq!(parse_windows(Some("1, 14,14")).unwrap(), vec![1, 14]);
        assert!(parse_windows(Some("0")).is_err());
        assert!(parse_windows(Some("91")).is_err());
        assert!(parse_windows(Some("abc")).is_err());
        assert!(parse_windows(Some("1,2,3,4,5,6,7")).is_err());
    }

    #[test]
    fn csv_export_has_header_and_one_line_per_row() {
        let mut r = PeriodRollup::empty(Dependency::Redis, end(), "host,1");
        r.calls = 3;
        let csv = rollups_to_csv(&[r]);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("dependency,period_start"));
        assert!(lines[1].starts_with("redis,2026-09-01T00:00:00+00:00,host_1,3,"));
        assert_eq!(lines[1].split(',').count(), lines[0].split(',').count());
    }

    #[test]
    fn instance_id_is_never_empty() {
        assert!(!instance_id().is_empty());
    }

    #[test]
    fn last_scorecard_cache_round_trips() {
        let card = build_scorecard(&[], end(), &[7]);
        set_last_scorecard(card);
        assert_eq!(last_scorecard_entries().len(), Dependency::ALL.len());
    }
}
