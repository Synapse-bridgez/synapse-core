use std::sync::Arc;
use tokio::sync::Mutex;
use redis::Client as RedisClient;
use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc, Duration};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CircuitBreakerError {
    #[error("Circuit breaker is open")]
    Open,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitBreakerState {
    pub state: CircuitState,
    pub opened_at: Option<DateTime<Utc>>,
    pub failure_count: u32,
    pub last_error: Option<String>,
}

/// A single observed request outcome for a dependency, recorded as it flows
/// through the circuit breaker. This is the raw signal the dependency health
/// scorecard aggregates over rolling windows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencySample {
    pub at: DateTime<Utc>,
    pub success: bool,
    pub latency_ms: u64,
    /// True when the failure looks like a partition on *our* side (e.g. the
    /// breaker fast-failed or the transport never reached the dependency)
    /// rather than an outage of the dependency itself. Both surface as failed
    /// requests to the caller but carry very different implications.
    pub self_side_partition: bool,
}

/// Rolling-window reliability summary for a single dependency.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyWindow {
    pub window_days: u32,
    pub total: u64,
    pub successes: u64,
    pub failures: u64,
    /// Failures attributable to the dependency itself (excludes self-side partitions).
    pub dependency_failures: u64,
    /// Failures attributable to a partition on our own side.
    pub self_side_partitions: u64,
    pub uptime: f64,
    pub error_rate: f64,
    pub latency_p50_ms: u64,
    pub latency_p95_ms: u64,
    pub latency_p99_ms: u64,
}

/// Full scorecard for one dependency across the standard rolling windows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DependencyScorecard {
    pub dependency: String,
    pub windows: Vec<DependencyWindow>,
}

/// The rolling windows the scorecard reports over.
const SCORECARD_WINDOWS_DAYS: [u32; 3] = [7, 30, 90];

#[derive(Clone)]
pub struct CircuitBreaker {
    service_name: String,
    redis_client: RedisClient,
    state: Arc<Mutex<CircuitBreakerState>>,
    failure_threshold: u32,
    reset_timeout: Duration,
    /// Bounded in-memory history of request outcomes used to derive the
    /// dependency health scorecard. Derived from existing breaker transitions
    /// and request results rather than new bespoke instrumentation.
    history: Arc<Mutex<Vec<DependencySample>>>,
}

impl CircuitBreaker {
    pub fn new(
        service_name: String,
        redis_client: RedisClient,
        failure_threshold: u32,
        reset_timeout: Duration,
    ) -> Self {
        let state = CircuitBreakerState {
            state: CircuitState::Closed,
            opened_at: None,
            failure_count: 0,
            last_error: None,
        };
        Self {
            service_name,
            redis_client,
            state: Arc::new(Mutex::new(state)),
            failure_threshold,
            reset_timeout,
            history: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub async fn load_from_redis(&self) -> Result<(), redis::RedisError> {
        let key = format!("cb:state:{}", self.service_name);
        let mut conn = self.redis_client.get_async_connection().await?;
        let data: Option<String> = redis::cmd("GET").arg(&key).query_async(&mut conn).await?;
        if let Some(json) = data {
            let persisted_state: CircuitBreakerState = serde_json::from_str(&json)?;
            *self.state.lock().await = persisted_state;
        }
        Ok(())
    }

    pub async fn call<F, Fut, T>(&self, f: F) -> Result<T, Box<dyn std::error::Error + Send + Sync>>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, Box<dyn std::error::Error + Send + Sync>>>,
    {
        let mut state = self.state.lock().await;
        match state.state {
            CircuitState::Open => {
                if Utc::now().signed_duration_since(state.opened_at.unwrap()) > self.reset_timeout {
                    state.state = CircuitState::HalfOpen;
                } else {
                    // Fast-fail: the request never reached the dependency, so this
                    // is a partition on our own side, not a dependency outage.
                    if let Some(dep) = self.scorecard_dependency() {
                        crate::services::dependency_scorecard::record_call(
                            dep,
                            crate::services::dependency_scorecard::CallOutcome::CircuitRejected,
                            std::time::Duration::ZERO,
                        );
                    }
                    drop(state);
                    self.record_sample(false, 0, true).await;
                    return Err(Box::new(CircuitBreakerError::Open));
                }
            }
            _ => {}
        }
        drop(state); // unlock

        let started = Utc::now();
        let result = f().await;
        let latency_ms = Utc::now()
            .signed_duration_since(started)
            .num_milliseconds()
            .max(0) as u64;

        let mut state = self.state.lock().await;
        match &result {
            Ok(_) => {
                if !matches!(state.state, CircuitState::Closed) {
                    if let Some(dep) = self.scorecard_dependency() {
                        crate::services::dependency_scorecard::record_circuit_transition(
                            dep,
                            crate::services::dependency_scorecard::CircuitTransition::Closed,
                        );
                    }
                }
                state.failure_count = 0;
                state.state = CircuitState::Closed;
                state.opened_at = None;
                state.last_error = None;
            }
            Err(e) => {
                state.failure_count += 1;
                state.last_error = Some(e.to_string());
                if state.failure_count >= self.failure_threshold {
                    // Re-opening from half-open is a new open period, too.
                    if !matches!(state.state, CircuitState::Open) {
                        if let Some(dep) = self.scorecard_dependency() {
                            crate::services::dependency_scorecard::record_circuit_transition(
                                dep,
                                crate::services::dependency_scorecard::CircuitTransition::Opened,
                            );
                        }
                    }
                    state.state = CircuitState::Open;
                    state.opened_at = Some(Utc::now());
                    // Persist
                    if let Err(persist_err) = self.persist_to_redis(&state).await {
                        tracing::error!("Failed to persist circuit breaker state: {}", persist_err);
                    }
                }
            }
        }
        drop(state);

        // A request that actually reached the dependency and failed is a
        // dependency-side failure, not a self-side partition.
        self.record_sample(result.is_ok(), latency_ms, false).await;
        result
    }

    /// Record a request outcome into the bounded scorecard history.
    async fn record_sample(&self, success: bool, latency_ms: u64, self_side_partition: bool) {
        let mut history = self.history.lock().await;
        history.push(DependencySample {
            at: Utc::now(),
            success,
            latency_ms,
            self_side_partition,
        });
        // Bound memory: keep at most the 90-day window worth of samples.
        let cutoff = Utc::now() - Duration::days(SCORECARD_WINDOWS_DAYS[2] as i64);
        history.retain(|s| s.at >= cutoff);
    }

    /// Build the dependency health scorecard across the standard rolling
    /// windows (7/30/90 days) from recorded request outcomes.
    pub async fn scorecard(&self) -> DependencyScorecard {
        let history = self.history.lock().await;
        let now = Utc::now();
        let windows = SCORECARD_WINDOWS_DAYS
            .iter()
            .map(|&days| aggregate_window(&history, now, days))
            .collect();
        DependencyScorecard {
            dependency: self.service_name.clone(),
            windows,
        }
    }

    async fn persist_to_redis(&self, state: &CircuitBreakerState) -> Result<(), redis::RedisError> {
        let key = format!("cb:state:{}", self.service_name);
        let json = serde_json::to_string(state)?;
        let mut conn = self.redis_client.get_async_connection().await?;
        redis::cmd("SETEX")
            .arg(&key)
            .arg(self.reset_timeout.num_seconds())
            .arg(json)
            .query_async(&mut conn)
            .await?;
        Ok(())
    }

    /// The scorecard dependency this breaker guards, if it guards one of the
    /// tracked dependencies (see `services::dependency_scorecard`).
    fn scorecard_dependency(&self) -> Option<crate::services::dependency_scorecard::Dependency> {
        crate::services::dependency_scorecard::Dependency::from_service_name(&self.service_name)
    }

    pub async fn get_state(&self) -> CircuitBreakerState {
        self.state.lock().await.clone()
    }
}

/// Aggregate a slice of samples into a rolling-window reliability summary.
///
/// Uptime and error rate are computed over requests that actually reached the
/// dependency; self-side partitions are reported separately so an outage is
/// never conflated with a partition on our own side.
fn aggregate_window(samples: &[DependencySample], now: DateTime<Utc>, days: u32) -> DependencyWindow {
    let cutoff = now - Duration::days(days as i64);
    let in_window: Vec<&DependencySample> = samples.iter().filter(|s| s.at >= cutoff).collect();

    let total = in_window.len() as u64;
    let successes = in_window.iter().filter(|s| s.success).count() as u64;
    let self_side_partitions = in_window.iter().filter(|s| s.self_side_partition).count() as u64;
    let failures = total - successes;
    let dependency_failures = failures.saturating_sub(self_side_partitions);

    // Reliability is measured against requests that reached the dependency.
    let reached = total.saturating_sub(self_side_partitions);
    let uptime = if reached == 0 {
        1.0
    } else {
        successes as f64 / reached as f64
    };
    let error_rate = if reached == 0 {
        0.0
    } else {
        dependency_failures as f64 / reached as f64
    };

    let mut latencies: Vec<u64> = in_window
        .iter()
        .filter(|s| !s.self_side_partition)
        .map(|s| s.latency_ms)
        .collect();
    latencies.sort_unstable();

    DependencyWindow {
        window_days: days,
        total,
        successes,
        failures,
        dependency_failures,
        self_side_partitions,
        uptime,
        error_rate,
        latency_p50_ms: percentile(&latencies, 50.0),
        latency_p95_ms: percentile(&latencies, 95.0),
        latency_p99_ms: percentile(&latencies, 99.0),
    }
}

/// Nearest-rank percentile over a pre-sorted slice.
fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted[idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a CircuitBreaker with an unreachable Redis URL.
    /// The client is lazy – construction succeeds; only actual I/O would fail.
    fn make_cb(threshold: u32, reset_secs: i64) -> CircuitBreaker {
        let client = RedisClient::open("redis://127.0.0.1:1/").unwrap();
        CircuitBreaker::new(
            "test-service".to_string(),
            client,
            threshold,
            Duration::seconds(reset_secs),
        )
    }

    fn fail() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Err("simulated failure".into())
    }

    fn ok() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(())
    }

    fn sample(days_ago: i64, success: bool, latency_ms: u64, self_side: bool) -> DependencySample {
        DependencySample {
            at: Utc::now() - Duration::days(days_ago),
            success,
            latency_ms,
            self_side_partition: self_side,
        }
    }

    // ── Closed → Open ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn closed_transitions_to_open_after_threshold() {
        let cb = make_cb(3, 60);

        // Two failures – still Closed
        for _ in 0..2 {
            let _ = cb.call(|| async { fail() }).await;
        }
        assert!(matches!(cb.get_state().await.state, CircuitState::Closed));

        // Third failure crosses threshold → Open
        let _ = cb.call(|| async { fail() }).await;
        let state = cb.get_state().await;
        assert!(matches!(state.state, CircuitState::Open));
        assert!(state.opened_at.is_some());
        assert_eq!(state.failure_count, 3);
    }

    // ── Open → HalfOpen ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn open_transitions_to_half_open_after_reset_timeout() {
        let cb = make_cb(1, 0); // reset_timeout = 0 s → expires immediately

        // Trip the breaker
        let _ = cb.call(|| async { fail() }).await;
        assert!(matches!(cb.get_state().await.state, CircuitState::Open));

        // Next call: timeout has elapsed → breaker moves to HalfOpen and the
        // inner function executes.  We return an error so it trips back to Open,
        // but the important thing is that the call was *attempted* (not fast-failed).
        let result = cb.call(|| async { fail() }).await;
        // The call was forwarded (not short-circuited with CircuitBreakerError::Open)
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert_ne!(err_msg, "Circuit breaker is open");
    }

    // ── HalfOpen → Closed ────────────────────────────────────────────────────

    #[tokio::test]
    async fn half_open_transitions_to_closed_on_success() {
        let cb = make_cb(1, 0); // reset_timeout = 0 s

        // Trip to Open
        let _ = cb.call(|| async { fail() }).await;

        // Probe succeeds → Closed
        let result = cb.call(|| async { ok() }).await;
        assert!(result.is_ok());

        let state = cb.get_state().await;
        assert!(matches!(state.state, CircuitState::Closed));
        assert_eq!(state.failure_count, 0);
        assert!(state.opened_at.is_none());
    }

    // ── HalfOpen → Open ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn half_open_transitions_to_open_on_failure() {
        let cb = make_cb(1, 0); // reset_timeout = 0 s

        // Trip to Open
        let _ = cb.call(|| async { fail() }).await;

        // Probe fails → back to Open
        let _ = cb.call(|| async { fail() }).await;

        let state = cb.get_state().await;
        assert!(matches!(state.state, CircuitState::Open));
        assert!(state.opened_at.is_some());
    }

    // ── Open fast-fails while timeout has not elapsed ─────────────────────────

    #[tokio::test]
    async fn open_fast_fails_before_reset_timeout() {
        let cb = make_cb(1, 3600); // reset_timeout = 1 hour

        // Trip to Open
        let _ = cb.call(|| async { fail() }).await;

        // Immediate call → fast-fail
        let result = cb.call(|| async { ok() }).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "Circuit breaker is open");
    }

    // ── Scorecard aggregation ────────────────────────────────────────────────

    #[test]
    fn aggregate_window_computes_uptime_error_rate_and_percentiles() {
        let now = Utc::now();
        let samples = vec![
            sample(1, true, 10, false),
            sample(1, true, 20, false),
            sample(1, true, 30, false),
            sample(1, false, 40, false),
            sample(1, false, 50, false),
        ];

        let w = aggregate_window(&samples, now, 7);
        assert_eq!(w.total, 5);
        assert_eq!(w.successes, 3);
        assert_eq!(w.failures, 2);
        assert_eq!(w.dependency_failures, 2);
        assert_eq!(w.self_side_partitions, 0);
        assert!((w.uptime - 0.6).abs() < 1e-9);
        assert!((w.error_rate - 0.4).abs() < 1e-9);
        assert_eq!(w.latency_p50_ms, 30);
        assert_eq!(w.latency_p95_ms, 50);
        assert_eq!(w.latency_p99_ms, 50);
    }

    #[test]
    fn aggregate_window_separates_self_side_partitions_from_outages() {
        let now = Utc::now();
        // 4 requests reached the dependency (3 ok, 1 failed) + 2 self-side partitions.
        let samples = vec![
            sample(1, true, 10, false),
            sample(1, true, 10, false),
            sample(1, true, 10, false),
            sample(1, false, 10, false),
            sample(1, false, 0, true),
            sample(1, false, 0, true),
        ];

        let w = aggregate_window(&samples, now, 7);
        assert_eq!(w.total, 6);
        assert_eq!(w.self_side_partitions, 2);
        assert_eq!(w.dependency_failures, 1);
        // Uptime/error rate measured only over requests that reached the dependency.
        assert!((w.uptime - 0.75).abs() < 1e-9);
        assert!((w.error_rate - 0.25).abs() < 1e-9);
        // Self-side partitions are excluded from latency percentiles.
        assert_eq!(w.latency_p50_ms, 10);
    }

    #[test]
    fn aggregate_window_respects_rolling_boundaries() {
        let now = Utc::now();
        let samples = vec![
            sample(1, true, 10, false),   // within 7d
            sample(10, true, 10, false),  // within 30d only
            sample(60, true, 10, false),  // within 90d only
            sample(120, true, 10, false), // outside all windows
        ];

        assert_eq!(aggregate_window(&samples, now, 7).total, 1);
        assert_eq!(aggregate_window(&samples, now, 30).total, 2);
        assert_eq!(aggregate_window(&samples, now, 90).total, 3);
    }

    #[test]
    fn aggregate_window_empty_is_fully_healthy() {
        let w = aggregate_window(&[], Utc::now(), 7);
        assert_eq!(w.total, 0);
        assert_eq!(w.uptime, 1.0);
        assert_eq!(w.error_rate, 0.0);
        assert_eq!(w.latency_p50_ms, 0);
    }

    #[tokio::test]
    async fn scorecard_reports_all_rolling_windows() {
        let cb = make_cb(5, 60);
        let _ = cb.call(|| async { ok() }).await;
        let _ = cb.call(|| async { fail() }).await;

        let card = cb.scorecard().await;
        assert_eq!(card.dependency, "test-service");
        assert_eq!(card.windows.len(), 3);
        assert_eq!(card.windows[0].window_days, 7);
        assert_eq!(card.windows[1].window_days, 30);
        assert_eq!(card.windows[2].window_days, 90);
        assert_eq!(card.windows[0].total, 2);
        assert_eq!(card.windows[0].successes, 1);
        assert_eq!(card.windows[0].dependency_failures, 1);
    }

    #[tokio::test]
    async fn fast_fail_is_recorded_as_self_side_partition() {
        let cb = make_cb(1, 3600);
        // Trip the breaker.
        let _ = cb.call(|| async { fail() }).await;
        // Fast-fail while open.
        let _ = cb.call(|| async { ok() }).await;

        let card = cb.scorecard().await;
        let w = &card.windows[0];
        assert_eq!(w.total, 2);
        assert_eq!(w.self_side_partitions, 1);
        assert_eq!(w.dependency_failures, 1);
    }
}
