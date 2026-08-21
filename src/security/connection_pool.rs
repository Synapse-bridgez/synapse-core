//! Bounded connection pool for the Security module.
//!
//! Provides a hard-capped, validated connection pool for security-layer
//! backends (e.g. Vault, auth service).  The design mirrors the telemetry
//! pool ([`crate::telemetry::connection_pool`]) so the two modules share the
//! same structural conventions.
//!
//! # Security guarantees
//!
//! - The backend URL is validated against an allow-list of safe schemes
//!   (`https` only for security backends) and a maximum length at construction
//!   time, preventing SSRF and injection vectors.
//! - Pool size is hard-capped at [`SecurityPoolConfig::max_size`]; acquisition
//!   attempts beyond this limit return [`SecurityPoolError::Exhausted`] rather
//!   than blocking or allocating unboundedly.
//! - Stale idle connections are evicted lazily on the next pool operation,
//!   preventing unbounded resource hold when the backend endpoint changes.
//! - Poisoned mutexes are recovered non-fatally so a panicking thread cannot
//!   permanently disable the security layer.
//!
//! # Usage
//!
//! ```text
//! use synapse_core::security::connection_pool::{SecurityConnectionPool, SecurityPoolConfig};
//!
//! let pool = SecurityConnectionPool::new()?;
//! let conn = pool.acquire()?;
//! // … use conn …
//! pool.release(conn);
//! ```

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::security::error::SecurityError;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum allowed length for a backend URL.
const MAX_ENDPOINT_LEN: usize = 2048;

/// Allowed URL schemes for security backends.
/// Only HTTPS is permitted; plain HTTP is rejected to prevent credential leakage.
const ALLOWED_SCHEMES: &[&str] = &["https://"];

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for the security-layer connection pool.
#[derive(Debug, Clone)]
pub struct SecurityPoolConfig {
    /// Maximum number of connections the pool may hold at once.
    ///
    /// Acquisition attempts beyond this limit return [`SecurityPoolError::Exhausted`].
    pub max_size: usize,

    /// Connections idle longer than this duration are evicted on the next operation.
    pub max_idle: Duration,

    /// Backend endpoint URL.
    ///
    /// Must use the `https://` scheme and must not exceed [`MAX_ENDPOINT_LEN`] characters.
    pub endpoint: String,
}

impl Default for SecurityPoolConfig {
    fn default() -> Self {
        Self {
            max_size: 5,
            max_idle: Duration::from_secs(120),
            endpoint: "https://localhost:8200".to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors produced by the security connection pool.
#[derive(Debug, thiserror::Error)]
pub enum SecurityPoolError {
    /// All connections are in use; the pool is at capacity.
    ///
    /// The caller should back off and retry, or fail the security operation
    /// gracefully.
    #[error("Security connection pool exhausted: all {0} connections in use")]
    Exhausted(usize),

    /// The pool configuration is invalid.
    ///
    /// Indicates `max_size` is zero, the endpoint scheme is not allowed, or
    /// the endpoint URL exceeds the maximum length.
    #[error("Invalid security pool configuration: {0}")]
    InvalidConfig(String),
}

impl From<SecurityPoolError> for SecurityError {
    fn from(e: SecurityPoolError) -> Self {
        // Pool errors are surfaced as rate-limit exceeded (429) because they
        // indicate the security backend is under load, not a client mistake.
        tracing::error!(error = %e, "Security connection pool error");
        SecurityError::RateLimitExceeded
    }
}

// ---------------------------------------------------------------------------
// Internal state
// ---------------------------------------------------------------------------

/// A single connection managed by the pool.
#[derive(Debug)]
pub struct SecurityConnection {
    /// Unique connection identifier within the pool.
    pub id: u64,
    /// Backend endpoint this connection targets.
    pub endpoint: String,
    last_used: Instant,
}

impl SecurityConnection {
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
    available: VecDeque<SecurityConnection>,
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

// ---------------------------------------------------------------------------
// Pool
// ---------------------------------------------------------------------------

/// Bounded, secure connection pool for security-layer backends.
///
/// See the [module-level documentation](self) for security guarantees.
#[derive(Debug, Clone)]
pub struct SecurityConnectionPool {
    config: SecurityPoolConfig,
    state: Arc<Mutex<PoolState>>,
}

impl SecurityConnectionPool {
    /// Creates a pool with default configuration.
    ///
    /// # Errors
    /// Returns [`SecurityPoolError::InvalidConfig`] if the default endpoint is invalid.
    pub fn new() -> Result<Self, SecurityPoolError> {
        Self::with_config(SecurityPoolConfig::default())
    }

    /// Creates a pool with the supplied configuration.
    ///
    /// Validates the endpoint URL and pool size at construction time, failing
    /// fast if configuration is invalid.
    ///
    /// # Errors
    /// - [`SecurityPoolError::InvalidConfig`] when `max_size` is zero.
    /// - [`SecurityPoolError::InvalidConfig`] when `endpoint` uses a disallowed scheme.
    /// - [`SecurityPoolError::InvalidConfig`] when `endpoint` exceeds [`MAX_ENDPOINT_LEN`].
    pub fn with_config(config: SecurityPoolConfig) -> Result<Self, SecurityPoolError> {
        validate_endpoint(&config.endpoint)?;

        if config.max_size == 0 {
            return Err(SecurityPoolError::InvalidConfig(
                "max_size must be at least 1".into(),
            ));
        }

        Ok(Self {
            config,
            state: Arc::new(Mutex::new(PoolState::new())),
        })
    }

    /// Acquires a connection from the pool.
    ///
    /// Stale idle connections are evicted before the availability check.
    /// Returns a fresh connection if the pool has capacity and no idle
    /// connections are available.
    ///
    /// # Errors
    /// [`SecurityPoolError::Exhausted`] when all `max_size` connections are in use.
    pub fn acquire(&self) -> Result<SecurityConnection, SecurityPoolError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.evict_stale_locked(&mut state);

        if let Some(conn) = state.available.pop_front() {
            return Ok(conn);
        }

        if state.total >= self.config.max_size {
            return Err(SecurityPoolError::Exhausted(self.config.max_size));
        }

        let id = state.next_id();
        state.total += 1;
        Ok(SecurityConnection::new(id, self.config.endpoint.clone()))
    }

    /// Returns a connection to the pool after use.
    ///
    /// Stale connections are discarded and the pool size is decremented.
    /// Non-stale connections are re-queued for future acquisition.
    /// Recovers gracefully from poisoned mutexes.
    pub fn release(&self, mut conn: SecurityConnection) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());

        if conn.is_stale(self.config.max_idle) {
            state.total = state.total.saturating_sub(1);
            return;
        }

        conn.touch();
        state.available.push_back(conn);
    }

    /// Number of idle connections currently available in the pool.
    pub fn idle_count(&self) -> usize {
        self.state.lock().map(|s| s.available.len()).unwrap_or(0)
    }

    /// Total connections managed by the pool (idle + currently in use).
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

// ---------------------------------------------------------------------------
// Endpoint validation
// ---------------------------------------------------------------------------

/// Validates a backend endpoint URL for use in the security pool.
///
/// Only `https://` scheme is accepted to prevent credential leakage over
/// plain HTTP.  The URL must not exceed [`MAX_ENDPOINT_LEN`] characters.
fn validate_endpoint(endpoint: &str) -> Result<(), SecurityPoolError> {
    if endpoint.is_empty() {
        return Err(SecurityPoolError::InvalidConfig(
            "endpoint must not be empty".into(),
        ));
    }

    if endpoint.len() > MAX_ENDPOINT_LEN {
        return Err(SecurityPoolError::InvalidConfig(format!(
            "endpoint exceeds maximum length of {} characters",
            MAX_ENDPOINT_LEN
        )));
    }

    let scheme_ok = ALLOWED_SCHEMES.iter().any(|s| endpoint.starts_with(s));
    if !scheme_ok {
        return Err(SecurityPoolError::InvalidConfig(
            "endpoint must use the https:// scheme".into(),
        ));
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- endpoint validation --

    #[test]
    fn https_endpoint_accepted() {
        assert!(validate_endpoint("https://vault.internal:8200").is_ok());
    }

    #[test]
    fn http_endpoint_rejected() {
        assert!(validate_endpoint("http://vault.internal:8200").is_err());
    }

    #[test]
    fn ftp_endpoint_rejected() {
        assert!(validate_endpoint("ftp://vault.internal:8200").is_err());
    }

    #[test]
    fn empty_endpoint_rejected() {
        assert!(validate_endpoint("").is_err());
    }

    #[test]
    fn endpoint_too_long_rejected() {
        let long = format!("https://{}", "a".repeat(MAX_ENDPOINT_LEN));
        assert!(validate_endpoint(&long).is_err());
    }

    // -- pool construction --

    #[test]
    fn default_pool_constructs_successfully() {
        assert!(SecurityConnectionPool::new().is_ok());
    }

    #[test]
    fn zero_max_size_rejected() {
        let config = SecurityPoolConfig {
            max_size: 0,
            ..Default::default()
        };
        assert!(SecurityConnectionPool::with_config(config).is_err());
    }

    #[test]
    fn invalid_endpoint_scheme_rejected_at_construction() {
        let config = SecurityPoolConfig {
            endpoint: "http://vault.internal:8200".to_string(),
            ..Default::default()
        };
        assert!(SecurityConnectionPool::with_config(config).is_err());
    }

    // -- acquire / release --

    #[test]
    fn acquire_creates_connection() {
        let pool = SecurityConnectionPool::new().unwrap();
        let conn = pool.acquire().unwrap();
        assert_eq!(conn.id, 1);
        assert_eq!(pool.total_count(), 1);
    }

    #[test]
    fn release_returns_connection_to_pool() {
        let pool = SecurityConnectionPool::new().unwrap();
        let conn = pool.acquire().unwrap();
        pool.release(conn);
        assert_eq!(pool.idle_count(), 1);
    }

    #[test]
    fn acquire_reuses_idle_connection() {
        let pool = SecurityConnectionPool::new().unwrap();
        let conn = pool.acquire().unwrap();
        let id = conn.id;
        pool.release(conn);
        let conn2 = pool.acquire().unwrap();
        assert_eq!(conn2.id, id);
        assert_eq!(pool.total_count(), 1);
    }

    #[test]
    fn exhausted_error_at_capacity() {
        let config = SecurityPoolConfig {
            max_size: 2,
            ..Default::default()
        };
        let pool = SecurityConnectionPool::with_config(config).unwrap();
        let _c1 = pool.acquire().unwrap();
        let _c2 = pool.acquire().unwrap();
        assert!(matches!(
            pool.acquire(),
            Err(SecurityPoolError::Exhausted(2))
        ));
    }

    #[test]
    fn stale_idle_connections_evicted_on_acquire() {
        let config = SecurityPoolConfig {
            max_idle: Duration::from_nanos(1),
            ..Default::default()
        };
        let pool = SecurityConnectionPool::with_config(config).unwrap();
        let conn = pool.acquire().unwrap();
        pool.release(conn);
        std::thread::sleep(Duration::from_millis(1));
        // Stale idle conn is evicted; a fresh one with a new id is created.
        let conn2 = pool.acquire().unwrap();
        assert_eq!(conn2.id, 2);
    }

    #[test]
    fn stale_release_decrements_total() {
        let config = SecurityPoolConfig {
            max_idle: Duration::from_nanos(1),
            ..Default::default()
        };
        let pool = SecurityConnectionPool::with_config(config).unwrap();
        let conn = pool.acquire().unwrap();
        assert_eq!(pool.total_count(), 1);
        std::thread::sleep(Duration::from_millis(1));
        pool.release(conn);
        assert_eq!(pool.total_count(), 0);
    }

    #[test]
    fn pool_error_converts_to_security_error() {
        let pool_err = SecurityPoolError::Exhausted(5);
        let _: SecurityError = pool_err.into();
    }
}
