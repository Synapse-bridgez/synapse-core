//! Secure connection pooling for telemetry exporters.
//!
//! Enforces a hard cap on pool size to prevent resource-exhaustion attacks,
//! validates endpoints at construction time, and evicts idle connections that
//! exceed the configured TTL.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::telemetry::error_handling::TelemetryError;
use crate::telemetry::input_validation::InputValidator;

/// Pool configuration for telemetry exporter connections.
///
/// # Health Check
///
/// Defines the constraints for the telemetry connection pool health check:
/// - `max_size` enforces a hard cap to prevent resource exhaustion attacks.
/// - `max_idle` evicts stale connections, preventing unbounded resource hold when the exporter changes.
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Maximum number of connections the pool may hold at once.
    pub max_size: usize,
    /// Connections idle longer than this duration are evicted on the next operation.
    pub max_idle: Duration,
    /// Exporter endpoint URL; validated against allowed schemes at construction time.
    pub endpoint: String,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_size: 10,
            max_idle: Duration::from_secs(300),
            endpoint: "http://localhost:4317".to_string(),
        }
    }
}

/// Configuration for the trend-based pool-exhaustion early warning.
///
/// Unlike a point-in-time "pool is at 100%" alert (a lagging indicator), this
/// fits a linear trend over a sliding window of utilization samples and
/// projects it forward by `forecast_window`. A projection crossing
/// `warning_threshold` fires the warning *before* the pool actually runs out,
/// giving operators time to react.
#[derive(Debug, Clone)]
pub struct EarlyWarningConfig {
    /// How far back to look when fitting the utilization trend. Wider windows
    /// smooth over brief spikes but react more slowly to real trends.
    pub window: Duration,
    /// How far ahead to project the fitted trend.
    pub forecast_window: Duration,
    /// Projected utilization (0.0-1.0) at which the warning fires.
    pub warning_threshold: f64,
    /// Minimum number of samples required before a trend is evaluated, to
    /// avoid firing on noise early in the pool's lifetime.
    pub min_samples: usize,
}

impl Default for EarlyWarningConfig {
    /// Defaults calibrated against `tests/load/` steady-ramp runs: a 2-minute
    /// window is long enough to average out a request-burst-driven spike
    /// (which recovers within seconds) while still catching a genuine
    /// utilization trend with enough of the 1-minute forecast window to
    /// spare for an operator to react. See `docs/pool_monitoring.md` for how
    /// to recalibrate per-deployment.
    fn default() -> Self {
        Self {
            window: Duration::from_secs(120),
            forecast_window: Duration::from_secs(60),
            warning_threshold: 0.9,
            min_samples: 4,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct UtilizationSample {
    at: Instant,
    utilization: f64,
}

/// Fits a linear trend over recent pool-utilization samples and forecasts
/// whether the pool is on a trajectory toward exhaustion.
///
/// Uses ordinary least-squares regression over the sliding window rather than
/// a full forecasting model, per the design goal of staying simple and
/// explainable. A spike-then-recover pattern nets out to a near-zero slope
/// over the window (utilization goes up, then back down), so it does not
/// trigger a warning — only a *sustained* upward trend does.
#[derive(Debug)]
pub struct ExhaustionForecaster {
    config: EarlyWarningConfig,
    samples: Mutex<VecDeque<UtilizationSample>>,
}

impl ExhaustionForecaster {
    pub fn new(config: EarlyWarningConfig) -> Self {
        Self {
            config,
            samples: Mutex::new(VecDeque::new()),
        }
    }

    /// Records a pool-utilization sample (0.0-1.0) at the given instant.
    /// Samples older than `config.window` are dropped.
    pub fn record_at(&self, utilization: f64, at: Instant) {
        let mut samples = self.samples.lock().unwrap_or_else(|e| e.into_inner());
        samples.push_back(UtilizationSample { at, utilization });
        let window = self.config.window;
        while let Some(front) = samples.front() {
            if at.saturating_duration_since(front.at) > window {
                samples.pop_front();
            } else {
                break;
            }
        }
    }

    /// Records a sample at the current time.
    pub fn record(&self, utilization: f64) {
        self.record_at(utilization, Instant::now());
    }

    /// Returns `Some(projected_utilization)` when a sustained growth trend
    /// forecasts the pool crossing `warning_threshold` within
    /// `forecast_window`. Returns `None` when there isn't enough data, the
    /// trend is flat/declining, or the projection stays under the threshold.
    pub fn evaluate(&self) -> Option<f64> {
        let samples = self.samples.lock().unwrap_or_else(|e| e.into_inner());
        if samples.len() < self.config.min_samples {
            return None;
        }

        let t0 = samples.front()?.at;
        let points: Vec<(f64, f64)> = samples
            .iter()
            .map(|s| (s.at.saturating_duration_since(t0).as_secs_f64(), s.utilization))
            .collect();

        let n = points.len() as f64;
        let mean_x = points.iter().map(|(x, _)| x).sum::<f64>() / n;
        let mean_y = points.iter().map(|(_, y)| y).sum::<f64>() / n;

        let mut num = 0.0;
        let mut den = 0.0;
        for (x, y) in &points {
            num += (x - mean_x) * (y - mean_y);
            den += (x - mean_x).powi(2);
        }

        if den == 0.0 {
            return None;
        }

        let slope = num / den; // utilization change per second
        if slope <= 0.0 {
            return None; // flat or declining: not a sustained growth trend
        }

        let intercept = mean_y - slope * mean_x;
        let last_x = points.last().map(|(x, _)| *x).unwrap_or(mean_x);
        let forecast_x = last_x + self.config.forecast_window.as_secs_f64();
        let projected = (intercept + slope * forecast_x).clamp(0.0, 1.0);

        if projected >= self.config.warning_threshold {
            Some(projected)
        } else {
            None
        }
    }
}

/// A single connection managed by the pool.
#[derive(Debug)]
pub struct PooledConnection {
    pub id: u64,
    pub endpoint: String,
    last_used: Instant,
}

impl PooledConnection {
    fn new(id: u64, endpoint: String) -> Self {
        Self {
            id,
            endpoint,
            last_used: Instant::now(),
        }
    }

    fn is_stale(&self, max_idle: Duration) -> bool {
        self.last_used.elapsed() > max_idle
    }

    fn touch(&mut self) {
        self.last_used = Instant::now();
    }
}

#[derive(Debug)]
struct PoolState {
    available: VecDeque<PooledConnection>,
    /// Total connections in existence (idle + currently in use).
    total: usize,
    next_id: u64,
}

impl PoolState {
    fn new() -> Self {
        Self {
            available: VecDeque::new(),
            total: 0,
            next_id: 1,
        }
    }

    fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

/// Bounded, secure connection pool for telemetry exporters.
///
/// # Security guarantees
///
/// - The endpoint URL is validated against an allow-list of safe schemes
///   (`http`/`https`) and a maximum length before any connection is created,
///   preventing SSRF and injection vectors.
/// - Pool size is hard-capped at [`PoolConfig::max_size`]; acquisition
///   attempts beyond this limit return [`PoolError::Exhausted`] rather than
///   blocking or allocating unboundedly — guarding against resource exhaustion.
/// - Stale idle connections are evicted lazily on the next pool operation,
///   preventing unbounded resource hold when a telemetry endpoint is replaced.
#[derive(Debug, Clone)]
pub struct ConnectionPool {
    config: PoolConfig,
    state: Arc<Mutex<PoolState>>,
    forecaster: Arc<ExhaustionForecaster>,
}

impl ConnectionPool {
    /// Creates a pool with default configuration.
    ///
    /// # Errors
    /// Returns [`TelemetryError::PoolConfigError`] if the default endpoint is invalid.
    pub fn new() -> Result<Self, TelemetryError> {
        Self::with_config(PoolConfig::default())
    }

    /// Creates a pool with the supplied configuration.
    ///
    /// Validates the endpoint URL and pool size at construction time, failing fast
    /// if configuration is invalid. This prevents resource exhaustion from invalid configs.
    ///
    /// # Errors
    /// - [`TelemetryError::ValidationError`] when `config.endpoint` is invalid.
    /// - [`TelemetryError::PoolConfigError`] when `max_size` is zero or invalid.
    pub fn with_config(config: PoolConfig) -> Result<Self, TelemetryError> {
        InputValidator::validate_endpoint(&config.endpoint)
            .map_err(TelemetryError::ValidationError)?;

        if config.max_size == 0 {
            return Err(TelemetryError::PoolConfigError(
                "max_size must be at least 1".into(),
            ));
        }

        Ok(Self {
            config,
            state: Arc::new(Mutex::new(PoolState::new())),
            forecaster: Arc::new(ExhaustionForecaster::new(EarlyWarningConfig::default())),
        })
    }

    fn record_utilization(&self, state: &PoolState) {
        let utilization = state.total as f64 / self.config.max_size as f64;
        self.forecaster.record(utilization);
    }

    /// Returns `Some(projected_utilization)` when utilization is on a
    /// sustained trajectory toward exhaustion within the forecaster's
    /// configured forecast window. See [`ExhaustionForecaster::evaluate`].
    ///
    /// This is a leading indicator — call it from a periodic health check
    /// alongside [`ConnectionPool::total_count`] to alert operators before
    /// [`TelemetryError::PoolExhausted`] actually occurs.
    pub fn forecast_exhaustion(&self) -> Option<f64> {
        self.forecaster.evaluate()
    }

    /// Acquires a connection from the pool for use.
    ///
    /// # Health Check
    ///
    /// A successful acquisition means the pool is healthy and has capacity. On success,
    /// the caller receives an idle connection from the queue or a newly created connection.
    /// Stale idle connections are evicted automatically before the availability check.
    ///
    /// A failing result with `Exhausted` means the pool has hit its capacity ceiling
    /// (`max_size`). The caller should back off and retry, or fail the telemetry operation
    /// gracefully (no-op degradation).
    ///
    /// # Errors
    /// [`TelemetryError::PoolExhausted`] when all `max_size` connections are in use.
    ///
    /// # Non-fatal behavior
    /// If the mutex is poisoned, recovers by creating a new state instead of panicking.
    pub fn acquire(&self) -> Result<PooledConnection, TelemetryError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.evict_stale_locked(&mut state);

        if let Some(conn) = state.available.pop_front() {
            self.record_utilization(&state);
            return Ok(conn);
        }

        if state.total >= self.config.max_size {
            return Err(TelemetryError::PoolExhausted(self.config.max_size));
        }

        let id = state.next_id();
        state.total += 1;
        self.record_utilization(&state);
        Ok(PooledConnection::new(id, self.config.endpoint.clone()))
    }

    /// Returns a connection to the pool after use.
    ///
    /// # Health Check
    ///
    /// Stale connections are discarded and the pool size is decremented.
    /// Non-stale connections are re-queued for future acquisition.
    /// Does not propagate errors; logs and recovers gracefully from poisoned mutexes.
    pub fn release(&self, mut conn: PooledConnection) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());

        if conn.is_stale(self.config.max_idle) {
            state.total = state.total.saturating_sub(1);
            self.record_utilization(&state);
            return;
        }

        conn.touch();
        state.available.push_back(conn);
        self.record_utilization(&state);
    }

    /// Number of idle connections currently in the pool.
    ///
    /// # Health Check
    ///
    /// Returns the count of idle connections available for reuse. A high idle count
    /// may indicate the exporter is slow or unavailable; zero idle count indicates
    /// all pool capacity is in use.
    pub fn idle_count(&self) -> usize {
        self.state.lock().map(|s| s.available.len()).unwrap_or(0)
    }

    /// Total connections managed by the pool (idle + currently in use).
    ///
    /// # Health Check
    ///
    /// Returns the total number of active and idle connections. If this equals `max_size`,
    /// the pool is at capacity and new acquisitions will fail with `Exhausted`.
    pub fn total_count(&self) -> usize {
        self.state.lock().map(|s| s.total).unwrap_or(0)
    }

    fn evict_stale_locked(&self, state: &mut PoolState) {
        let max_idle = self.config.max_idle;
        let before = state.available.len();
        state.available.retain(|c| !c.is_stale(max_idle));
        let evicted = before - state.available.len();
        state.total = state.total.saturating_sub(evicted);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_acquire_creates_connection() {
        let pool = ConnectionPool::new().unwrap();
        let conn = pool.acquire().unwrap();
        assert_eq!(conn.id, 1);
        assert_eq!(pool.total_count(), 1);
    }

    #[test]
    fn test_release_returns_connection_to_pool() {
        let pool = ConnectionPool::new().unwrap();
        let conn = pool.acquire().unwrap();
        pool.release(conn);
        assert_eq!(pool.idle_count(), 1);
    }

    #[test]
    fn test_acquire_reuses_idle_connection() {
        let pool = ConnectionPool::new().unwrap();
        let conn = pool.acquire().unwrap();
        let id = conn.id;
        pool.release(conn);
        let conn2 = pool.acquire().unwrap();
        assert_eq!(conn2.id, id);
        assert_eq!(pool.total_count(), 1);
    }

    #[test]
    fn test_exhausted_error_at_capacity() {
        let config = PoolConfig {
            max_size: 2,
            ..Default::default()
        };
        let pool = ConnectionPool::with_config(config).unwrap();
        let _c1 = pool.acquire().unwrap();
        let _c2 = pool.acquire().unwrap();
        assert!(matches!(
            pool.acquire(),
            Err(TelemetryError::PoolExhausted(2))
        ));
    }

    #[test]
    fn test_stale_idle_connections_are_evicted_on_acquire() {
        let config = PoolConfig {
            max_idle: Duration::from_nanos(1),
            ..Default::default()
        };
        let pool = ConnectionPool::with_config(config).unwrap();
        let conn = pool.acquire().unwrap();
        pool.release(conn);
        std::thread::sleep(Duration::from_millis(1));
        // Stale idle conn is evicted; a fresh one with a new id is created.
        let conn2 = pool.acquire().unwrap();
        assert_eq!(conn2.id, 2);
    }

    #[test]
    fn test_stale_release_decrements_total() {
        let config = PoolConfig {
            max_idle: Duration::from_nanos(1),
            ..Default::default()
        };
        let pool = ConnectionPool::with_config(config).unwrap();
        let conn = pool.acquire().unwrap();
        assert_eq!(pool.total_count(), 1);
        std::thread::sleep(Duration::from_millis(1));
        pool.release(conn);
        assert_eq!(pool.total_count(), 0);
    }

    #[test]
    fn test_invalid_endpoint_scheme_rejected() {
        let config = PoolConfig {
            endpoint: "ftp://exporter:4317".to_string(),
            ..Default::default()
        };
        assert!(ConnectionPool::with_config(config).is_err());
    }

    #[test]
    fn test_zero_max_size_rejected() {
        let config = PoolConfig {
            max_size: 0,
            ..Default::default()
        };
        assert!(ConnectionPool::with_config(config).is_err());
    }

    fn config_for_test() -> EarlyWarningConfig {
        EarlyWarningConfig {
            window: Duration::from_secs(120),
            forecast_window: Duration::from_secs(60),
            warning_threshold: 0.9,
            min_samples: 4,
        }
    }

    #[test]
    fn early_warning_fires_on_steady_growth_trend() {
        let forecaster = ExhaustionForecaster::new(config_for_test());
        let t0 = Instant::now();
        // Utilization climbs from 40% to 82% over 100s at a steady rate;
        // projecting 60s further crosses the 90% threshold.
        for i in 0..=10 {
            let utilization = 0.40 + (i as f64) * 0.042;
            forecaster.record_at(utilization, t0 + Duration::from_secs(i * 10));
        }
        assert!(
            forecaster.evaluate().is_some(),
            "sustained steady growth should trigger the early warning"
        );
    }

    #[test]
    fn early_warning_does_not_fire_on_spike_then_recover() {
        let forecaster = ExhaustionForecaster::new(config_for_test());
        let t0 = Instant::now();
        // A brief burst pushes utilization to 95% then it recovers to 45% -
        // net trend over the window is roughly flat, not a sustained climb.
        let series = [0.45, 0.60, 0.95, 0.90, 0.70, 0.50, 0.45, 0.46];
        for (i, utilization) in series.iter().enumerate() {
            forecaster.record_at(*utilization, t0 + Duration::from_secs(i as u64 * 10));
        }
        assert!(
            forecaster.evaluate().is_none(),
            "a spike that recovers must not trigger the early warning"
        );
    }

    #[test]
    fn early_warning_does_not_fire_below_min_samples() {
        let forecaster = ExhaustionForecaster::new(config_for_test());
        let t0 = Instant::now();
        forecaster.record_at(0.95, t0);
        forecaster.record_at(0.97, t0 + Duration::from_secs(10));
        assert!(
            forecaster.evaluate().is_none(),
            "must not evaluate a trend from too few samples"
        );
    }

    #[test]
    fn early_warning_does_not_fire_on_declining_trend() {
        let forecaster = ExhaustionForecaster::new(config_for_test());
        let t0 = Instant::now();
        for i in 0..=10 {
            let utilization = 0.95 - (i as f64) * 0.05;
            forecaster.record_at(utilization, t0 + Duration::from_secs(i * 10));
        }
        assert!(
            forecaster.evaluate().is_none(),
            "a declining trend must not trigger the early warning"
        );
    }

    #[test]
    fn pool_forecast_exhaustion_reflects_sustained_acquisition_growth() {
        let config = PoolConfig {
            max_size: 10,
            ..Default::default()
        };
        let pool = ConnectionPool::with_config(config).unwrap();
        let t0 = Instant::now();
        // Simulate steady, sustained growth toward the pool's capacity by
        // feeding the forecaster directly (acquire()/release() always sample
        // "now", so we drive the forecaster the same way the pool does).
        for i in 0..=10 {
            let utilization = 0.30 + (i as f64) * 0.06;
            pool.forecaster.record_at(utilization, t0 + Duration::from_secs(i * 10));
        }
        assert!(pool.forecast_exhaustion().is_some());
    }

    #[test]
    fn test_https_endpoint_accepted() {
        let config = PoolConfig {
            endpoint: "https://otel-collector.internal:4317".to_string(),
            ..Default::default()
        };
        assert!(ConnectionPool::with_config(config).is_ok());
    }
}
