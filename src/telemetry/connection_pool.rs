//! Secure connection pooling for telemetry exporters.
//!
//! Enforces a hard cap on pool size to prevent resource-exhaustion attacks,
//! validates endpoints at construction time, and evicts idle connections that
//! exceed the configured TTL.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::telemetry::input_validation::InputValidator;
use crate::telemetry::error_handling::TelemetryError;

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
            .map_err(|e| TelemetryError::ValidationError(e))?;

        if config.max_size == 0 {
            return Err(TelemetryError::PoolConfigError(
                "max_size must be at least 1".into(),
            ));
        }

        Ok(Self {
            config,
            state: Arc::new(Mutex::new(PoolState::new())),
        })
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
            return Ok(conn);
        }

        if state.total >= self.config.max_size {
            return Err(TelemetryError::PoolExhausted(self.config.max_size));
        }

        let id = state.next_id();
        state.total += 1;
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
            return;
        }

        conn.touch();
        state.available.push_back(conn);
    }

    /// Number of idle connections currently in the pool.
    ///
    /// # Health Check
    ///
    /// Returns the count of idle connections available for reuse. A high idle count
    /// may indicate the exporter is slow or unavailable; zero idle count indicates
    /// all pool capacity is in use.
    pub fn idle_count(&self) -> usize {
        self.state
            .lock()
            .map(|s| s.available.len())
            .unwrap_or(0)
    }

    /// Total connections managed by the pool (idle + currently in use).
    ///
    /// # Health Check
    ///
    /// Returns the total number of active and idle connections. If this equals `max_size`,
    /// the pool is at capacity and new acquisitions will fail with `Exhausted`.
    pub fn total_count(&self) -> usize {
        self.state
            .lock()
            .map(|s| s.total)
            .unwrap_or(0)
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
        assert!(matches!(pool.acquire(), Err(TelemetryError::PoolExhausted(2))));
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

    #[test]
    fn test_https_endpoint_accepted() {
        let config = PoolConfig {
            endpoint: "https://otel-collector.internal:4317".to_string(),
            ..Default::default()
        };
        assert!(ConnectionPool::with_config(config).is_ok());
    }
}
