//! Shared "Redis degraded mode" signal (issue #1335).
//!
//! Every Redis-dependent component reports a Redis failure it degraded
//! around through [`record_redis_degraded`], which emits the *same* three
//! signals for all of them:
//!
//! - metric `redis_degraded_operations_total{component, fallback}`
//! - a `WARN` log with the fixed field set
//!   `degraded_mode=true dependency="redis" component=… fallback=…`
//!   (rate-limited per component so an outage cannot flood logs; the
//!   suppressed count is carried on the next line)
//! - an in-process per-component snapshot ([`snapshot`]) surfaced on
//!   `/ready`, so the full blast radius of an outage in progress is visible
//!   in one place.
//!
//! See `docs/redis-degradation.md` for the per-path audit.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::Duration;

use serde::Serialize;

/// Every Redis-dependent component. Bounded, fixed set — safe as a label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RedisComponent {
    QueryCache,
    RateLimit,
    Idempotency,
    Session,
    WebhookDispatcher,
    WebhookFilterCache,
    SecretsRotationPubSub,
}

impl RedisComponent {
    pub const ALL: [RedisComponent; 7] = [
        RedisComponent::QueryCache,
        RedisComponent::RateLimit,
        RedisComponent::Idempotency,
        RedisComponent::Session,
        RedisComponent::WebhookDispatcher,
        RedisComponent::WebhookFilterCache,
        RedisComponent::SecretsRotationPubSub,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            RedisComponent::QueryCache => "query_cache",
            RedisComponent::RateLimit => "rate_limit",
            RedisComponent::Idempotency => "idempotency",
            RedisComponent::Session => "session",
            RedisComponent::WebhookDispatcher => "webhook_dispatcher",
            RedisComponent::WebhookFilterCache => "webhook_filter_cache",
            RedisComponent::SecretsRotationPubSub => "secrets_rotation_pubsub",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|c| *c == self).unwrap_or(0)
    }
}

/// What the component did instead of failing the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DegradedFallback {
    /// Cache miss → read straight from Postgres.
    DirectDbRead,
    /// Served/kept in the per-process in-memory cache only.
    InMemoryOnly,
    /// Enforced a process-local limit *stricter* than normal.
    StricterLocalLimit,
    /// Used the Postgres-backed fallback store.
    DbFallback,
    /// Best-effort side effect (invalidation, announcement) skipped.
    SkippedBestEffort,
    /// Fell back to periodic polling.
    PollOnly,
    /// Refused the operation safely (e.g. 503) rather than guessing.
    FailClosed,
}

impl DegradedFallback {
    pub fn as_str(self) -> &'static str {
        match self {
            DegradedFallback::DirectDbRead => "direct_db_read",
            DegradedFallback::InMemoryOnly => "in_memory_only",
            DegradedFallback::StricterLocalLimit => "stricter_local_limit",
            DegradedFallback::DbFallback => "db_fallback",
            DegradedFallback::SkippedBestEffort => "skipped_best_effort",
            DegradedFallback::PollOnly => "poll_only",
            DegradedFallback::FailClosed => "fail_closed",
        }
    }
}

/// A component is reported "active" on `/ready` if it degraded within this
/// long.
pub const ACTIVE_WINDOW: Duration = Duration::from_secs(60);
/// At most one WARN line per component per this interval.
const LOG_INTERVAL_SECS: i64 = 10;

struct ComponentState {
    events: AtomicU64,
    last_unix_ms: AtomicI64,
    last_log_unix_ms: AtomicI64,
    suppressed: AtomicU64,
    last_fallback: AtomicU64,
}

impl ComponentState {
    const fn new() -> Self {
        Self {
            events: AtomicU64::new(0),
            last_unix_ms: AtomicI64::new(0),
            last_log_unix_ms: AtomicI64::new(0),
            suppressed: AtomicU64::new(0),
            last_fallback: AtomicU64::new(0),
        }
    }
}

static STATES: [ComponentState; 7] = [
    ComponentState::new(),
    ComponentState::new(),
    ComponentState::new(),
    ComponentState::new(),
    ComponentState::new(),
    ComponentState::new(),
    ComponentState::new(),
];

const FALLBACKS: [DegradedFallback; 7] = [
    DegradedFallback::DirectDbRead,
    DegradedFallback::InMemoryOnly,
    DegradedFallback::StricterLocalLimit,
    DegradedFallback::DbFallback,
    DegradedFallback::SkippedBestEffort,
    DegradedFallback::PollOnly,
    DegradedFallback::FailClosed,
];

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Records that `component` hit a Redis failure and degraded to `fallback`.
/// The single entry point every Redis-dependent path uses.
pub fn record_redis_degraded(
    component: RedisComponent,
    fallback: DegradedFallback,
    error: &dyn std::fmt::Display,
) {
    record_at(component, fallback, error, now_ms());
}

fn record_at(
    component: RedisComponent,
    fallback: DegradedFallback,
    error: &dyn std::fmt::Display,
    at_ms: i64,
) {
    record_into(
        &STATES[component.index()],
        component,
        fallback,
        error,
        at_ms,
    );
}

fn record_into(
    state: &ComponentState,
    component: RedisComponent,
    fallback: DegradedFallback,
    error: &dyn std::fmt::Display,
    at_ms: i64,
) {
    state.events.fetch_add(1, Ordering::Relaxed);
    state.last_unix_ms.store(at_ms, Ordering::Relaxed);
    let fallback_idx = FALLBACKS.iter().position(|f| *f == fallback).unwrap_or(0);
    state
        .last_fallback
        .store(fallback_idx as u64, Ordering::Relaxed);

    crate::metrics::redis_degraded_operations_total().add(
        1,
        &[
            opentelemetry::KeyValue::new("component", component.as_str()),
            opentelemetry::KeyValue::new("fallback", fallback.as_str()),
        ],
    );

    let last_log = state.last_log_unix_ms.load(Ordering::Relaxed);
    let due = at_ms - last_log >= LOG_INTERVAL_SECS * 1000;
    if due
        && state
            .last_log_unix_ms
            .compare_exchange(last_log, at_ms, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    {
        let suppressed = state.suppressed.swap(0, Ordering::Relaxed);
        tracing::warn!(
            degraded_mode = true,
            dependency = "redis",
            component = component.as_str(),
            fallback = fallback.as_str(),
            suppressed_since_last_log = suppressed,
            error = %error,
            "Redis unavailable: component running in degraded mode"
        );
    } else {
        state.suppressed.fetch_add(1, Ordering::Relaxed);
    }
}

/// One component's degradation state.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ComponentDegradation {
    pub component: RedisComponent,
    pub fallback: DegradedFallback,
    pub events_total: u64,
    pub last_degraded_at_unix_ms: i64,
    /// Degraded within [`ACTIVE_WINDOW`].
    pub active: bool,
}

/// Every component that has ever degraded in this process.
pub fn snapshot() -> Vec<ComponentDegradation> {
    snapshot_at(now_ms())
}

fn snapshot_at(now: i64) -> Vec<ComponentDegradation> {
    RedisComponent::ALL
        .iter()
        .filter_map(|&component| {
            let s = &STATES[component.index()];
            let events_total = s.events.load(Ordering::Relaxed);
            if events_total == 0 {
                return None;
            }
            let last = s.last_unix_ms.load(Ordering::Relaxed);
            Some(ComponentDegradation {
                component,
                fallback: FALLBACKS[s.last_fallback.load(Ordering::Relaxed) as usize % 7],
                events_total,
                last_degraded_at_unix_ms: last,
                active: now - last <= ACTIVE_WINDOW.as_millis() as i64,
            })
        })
        .collect()
}

/// Components currently degraded (within [`ACTIVE_WINDOW`]).
pub fn active_components() -> Vec<RedisComponent> {
    snapshot()
        .into_iter()
        .filter(|c| c.active)
        .map(|c| c.component)
        .collect()
}

/// Total degraded events recorded for `component` (test/diagnostic helper).
pub fn events_for(component: RedisComponent) -> u64 {
    STATES[component.index()].events.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_stable_and_unique() {
        let names: std::collections::HashSet<_> =
            RedisComponent::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(names.len(), RedisComponent::ALL.len());
        let fallbacks: std::collections::HashSet<_> =
            FALLBACKS.iter().map(|f| f.as_str()).collect();
        assert_eq!(fallbacks.len(), FALLBACKS.len());
    }

    #[test]
    fn recording_updates_snapshot_and_activity_window() {
        let comp = RedisComponent::WebhookFilterCache;
        let before = events_for(comp);
        let t = now_ms();
        record_at(comp, DegradedFallback::SkippedBestEffort, &"boom", t);
        record_at(comp, DegradedFallback::SkippedBestEffort, &"boom", t + 1);
        assert_eq!(events_for(comp), before + 2);

        let entry = snapshot_at(t + 10)
            .into_iter()
            .find(|c| c.component == comp)
            .unwrap();
        assert!(entry.active);
        assert_eq!(entry.fallback, DegradedFallback::SkippedBestEffort);
        assert!(entry.last_degraded_at_unix_ms >= t);

        // Long after: still listed (history), but no longer active.
        let later = snapshot_at(t + ACTIVE_WINDOW.as_millis() as i64 + 5_000)
            .into_iter()
            .find(|c| c.component == comp)
            .unwrap();
        assert!(!later.active);
    }

    #[test]
    fn log_rate_limit_counts_suppressed_lines() {
        let s = ComponentState::new();
        let comp = RedisComponent::SecretsRotationPubSub;
        let t = now_ms();
        record_into(&s, comp, DegradedFallback::PollOnly, &"x", t);
        assert_eq!(s.suppressed.load(Ordering::Relaxed), 0);
        record_into(&s, comp, DegradedFallback::PollOnly, &"x", t + 100);
        record_into(&s, comp, DegradedFallback::PollOnly, &"x", t + 200);
        assert_eq!(s.suppressed.load(Ordering::Relaxed), 2);
        assert_eq!(s.events.load(Ordering::Relaxed), 3);
        // After the interval a line is emitted and the counter resets.
        record_into(
            &s,
            comp,
            DegradedFallback::PollOnly,
            &"x",
            t + LOG_INTERVAL_SECS * 1000 + 1,
        );
        assert_eq!(s.suppressed.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn active_components_lists_recent_only() {
        record_redis_degraded(
            RedisComponent::WebhookDispatcher,
            DegradedFallback::SkippedBestEffort,
            &"down",
        );
        assert!(active_components().contains(&RedisComponent::WebhookDispatcher));
        assert!(!snapshot().is_empty());
    }
}
