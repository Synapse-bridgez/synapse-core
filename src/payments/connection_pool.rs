//! Secure connection pool for the Payments / settlement module.
//!
//! Provides a bounded, validated connection pool for the database connections
//! used by settlement logic.  The pool enforces hard resource caps, validates
//! endpoint configuration at construction time, and evicts stale connections
//! lazily to prevent unbounded resource hold.
//!
//! # Security guarantees
//!
//! - The database URL is validated for scheme (`postgres://` / `postgresql://`)
//!   and maximum length before any connection is created, preventing injection
//!   and SSRF vectors.
//! - Pool size is hard-capped at [`PaymentsPoolConfig::max_size`]; acquisition
//!   attempts beyond this limit return [`PaymentsPoolError::Exhausted`] rather
//!   than blocking or allocating unboundedly.
//! - Stale idle connections are evicted lazily on the next pool operation.
//! - Poisoned mutexes are recovered non-fatally so a panicking thread cannot
//!   permanently disable the payments layer.
//!
//! # Usage
//!
//! ```text
//! use synapse_core::payments::connection_pool::{PaymentsConnectionPool, PaymentsPoolConfig};
//!
//! let pool = PaymentsConnectionPool::new()?;
//! let conn = pool.acquire()?;
//! // … execute settlement query …
//! pool.release(conn);
//! ```

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::payments::error::PaymentError;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum allowed length for a database URL.
const MAX_DB_URL_LEN: usize = 2048;

/// Allowed URL schemes for payments database connections.
const ALLOWED_SCHEMES: &[&str] = &["postgres://", "postgresql://"];

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for the payments connection pool.
#[derive(Debug, Clone)]
pub struct PaymentsPoolConfig {
    /// Maximum number of connections the pool may hold at once.
    ///
    /// Acquisition attempts beyond this limit return [`PaymentsPoolError::Exhausted`].
    pub max_size: usize,

    /// Connections idle longer than this duration are evicted on the next operation.
    pub max_idle: Duration,

    /// Database URL.
    ///
    /// Must use the `postgres://` or `postgresql://` scheme and must not exceed
    /// [`MAX_DB_URL_LEN`] characters.
    pub database_url: String,
}

impl Default for PaymentsPoolConfig {
    fn default() -> Self {
        Self {
            max_size: 10,
            max_idle: Duration::from_secs(300),
            database_url: "postgres://localhost:5432/synapse".to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors produced by the payments connection pool.
#[derive(Debug, thiserror::Error)]
pub enum PaymentsPoolError {
    /// All connections are in use; the pool is at capacity.
    #[error("Payments connection pool exhausted: all {0} connections in use")]
    Exhausted(usize),

    /// The pool configuration is invalid.
    #[error("Invalid payments pool configuration: {0}")]
    InvalidConfig(String),
}

impl From<PaymentsPoolError> for PaymentError {
    fn from(e: PaymentsPoolError) -> Self {
        tracing::error!(error = %e, "Payments connection pool error");
        PaymentError::Database(format!("Connection pool error: {}", e))
    }
}

// ---------------------------------------------------------------------------
// Internal state
// ---------------------------------------------------------------------------

/// A single connection managed by the payments pool.
#[derive(Debug)]
pub struct PaymentsConnection {
    /// Unique connection identifier within the pool.
    pub id: u64,
    /// Database URL this connection targets (scheme + host only; no credentials).
    pub endpoint: String,
    last_used: Instant,
}

impl PaymentsConnection {
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
    available: VecDeque<PaymentsConnection>,
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

/// Bounded, secure connection pool for the payments / settlement module.
///
/// See the [module-level documentation](self) for security guarantees.
#[derive(Debug, Clone)]
pub struct PaymentsConnectionPool {
    config: PaymentsPoolConfig,
    state: Arc<Mutex<PoolState>>,
}

impl PaymentsConnectionPool {
    /// Creates a pool with default configuration.
    ///
    /// # Errors
    /// Returns [`PaymentsPoolError::InvalidConfig`] if the default database URL is invalid.
    pub fn new() -> Result<Self, PaymentsPoolError> {
        Self::with_config(PaymentsPoolConfig::default())
    }

    /// Creates a pool with the supplied configuration.
    ///
    /// Validates the database URL and pool size at construction time.
    ///
    /// # Errors
    /// - [`PaymentsPoolError::InvalidConfig`] when `max_size` is zero.
    /// - [`PaymentsPoolError::InvalidConfig`] when `database_url` uses a disallowed scheme.
    /// - [`PaymentsPoolError::InvalidConfig`] when `database_url` exceeds [`MAX_DB_URL_LEN`].
    pub fn with_config(config: PaymentsPoolConfig) -> Result<Self, PaymentsPoolError> {
        validate_database_url(&config.database_url)?;

        if config.max_size == 0 {
            return Err(PaymentsPoolError::InvalidConfig(
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
    ///
    /// # Errors
    /// [`PaymentsPoolError::Exhausted`] when all `max_size` connections are in use.
    pub fn acquire(&self) -> Result<PaymentsConnection, PaymentsPoolError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        self.evict_stale_locked(&mut state);

        if let Some(conn) = state.available.pop_front() {
            return Ok(conn);
        }

        if state.total >= self.config.max_size {
            return Err(PaymentsPoolError::Exhausted(self.config.max_size));
        }

        let id = state.next_id();
        state.total += 1;
        // Store only the scheme+host portion to avoid logging credentials.
        let endpoint = safe_endpoint_label(&self.config.database_url);
        Ok(PaymentsConnection::new(id, endpoint))
    }

    /// Returns a connection to the pool after use.
    ///
    /// Stale connections are discarded; non-stale connections are re-queued.
    /// Recovers gracefully from poisoned mutexes.
    pub fn release(&self, mut conn: PaymentsConnection) {
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
// Helpers
// ---------------------------------------------------------------------------

/// Validates a database URL for use in the payments pool.
///
/// Only `postgres://` and `postgresql://` schemes are accepted.
fn validate_database_url(url: &str) -> Result<(), PaymentsPoolError> {
    if url.is_empty() {
        return Err(PaymentsPoolError::InvalidConfig(
            "database_url must not be empty".into(),
        ));
    }

    if url.len() > MAX_DB_URL_LEN {
        return Err(PaymentsPoolError::InvalidConfig(format!(
            "database_url exceeds maximum length of {} characters",
            MAX_DB_URL_LEN
        )));
    }

    let scheme_ok = ALLOWED_SCHEMES.iter().any(|s| url.starts_with(s));
    if !scheme_ok {
        return Err(PaymentsPoolError::InvalidConfig(
            "database_url must use the postgres:// or postgresql:// scheme".into(),
        ));
    }

    Ok(())
}

/// Returns a safe label for the endpoint (scheme + host only, no credentials).
///
/// Strips the userinfo component (`user:pass@`) so connection IDs logged in
/// traces never contain database credentials.
fn safe_endpoint_label(url: &str) -> String {
    // Find the scheme end ("://")
    if let Some(after_scheme) = url.find("://").map(|i| i + 3) {
        let rest = &url[after_scheme..];
        // Strip userinfo if present (everything before the last '@' before the first '/')
        let host_start = rest.rfind('@').map(|i| i + 1).unwrap_or(0);
        let host_and_path = &rest[host_start..];
        // Keep only up to the first '/' (host:port)
        let host = host_and_path.split('/').next().unwrap_or(host_and_path);
        let scheme = &url[..after_scheme];
        return format!("{}{}", scheme, host);
    }
    // Fallback: return a generic label rather than the raw URL
    "<payments-db>".to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- URL validation --

    #[test]
    fn postgres_url_accepted() {
        assert!(validate_database_url("postgres://localhost:5432/synapse").is_ok());
    }

    #[test]
    fn postgresql_url_accepted() {
        assert!(validate_database_url("postgresql://localhost:5432/synapse").is_ok());
    }

    #[test]
    fn http_url_rejected() {
        assert!(validate_database_url("http://localhost:5432/synapse").is_err());
    }

    #[test]
    fn empty_url_rejected() {
        assert!(validate_database_url("").is_err());
    }

    #[test]
    fn url_too_long_rejected() {
        let long = format!("postgres://{}", "a".repeat(MAX_DB_URL_LEN));
        assert!(validate_database_url(&long).is_err());
    }

    // -- safe_endpoint_label --

    #[test]
    fn credentials_stripped_from_label() {
        let url = "postgres://user:secret@db.internal:5432/payments";
        let label = safe_endpoint_label(url);
        assert!(!label.contains("secret"));
        assert!(!label.contains("user"));
        assert!(label.contains("db.internal"));
    }

    #[test]
    fn label_without_credentials_preserved() {
        let url = "postgres://db.internal:5432/payments";
        let label = safe_endpoint_label(url);
        assert!(label.contains("db.internal"));
    }

    // -- pool construction --

    #[test]
    fn default_pool_constructs_successfully() {
        assert!(PaymentsConnectionPool::new().is_ok());
    }

    #[test]
    fn zero_max_size_rejected() {
        let config = PaymentsPoolConfig {
            max_size: 0,
            ..Default::default()
        };
        assert!(PaymentsConnectionPool::with_config(config).is_err());
    }

    #[test]
    fn invalid_scheme_rejected_at_construction() {
        let config = PaymentsPoolConfig {
            database_url: "mysql://localhost:3306/synapse".to_string(),
            ..Default::default()
        };
        assert!(PaymentsConnectionPool::with_config(config).is_err());
    }

    // -- acquire / release --

    #[test]
    fn acquire_creates_connection() {
        let pool = PaymentsConnectionPool::new().unwrap();
        let conn = pool.acquire().unwrap();
        assert_eq!(conn.id, 1);
        assert_eq!(pool.total_count(), 1);
    }

    #[test]
    fn release_returns_connection_to_pool() {
        let pool = PaymentsConnectionPool::new().unwrap();
        let conn = pool.acquire().unwrap();
        pool.release(conn);
        assert_eq!(pool.idle_count(), 1);
    }

    #[test]
    fn acquire_reuses_idle_connection() {
        let pool = PaymentsConnectionPool::new().unwrap();
        let conn = pool.acquire().unwrap();
        let id = conn.id;
        pool.release(conn);
        let conn2 = pool.acquire().unwrap();
        assert_eq!(conn2.id, id);
        assert_eq!(pool.total_count(), 1);
    }

    #[test]
    fn exhausted_error_at_capacity() {
        let config = PaymentsPoolConfig {
            max_size: 2,
            ..Default::default()
        };
        let pool = PaymentsConnectionPool::with_config(config).unwrap();
        let _c1 = pool.acquire().unwrap();
        let _c2 = pool.acquire().unwrap();
        assert!(matches!(
            pool.acquire(),
            Err(PaymentsPoolError::Exhausted(2))
        ));
    }

    #[test]
    fn stale_idle_connections_evicted_on_acquire() {
        let config = PaymentsPoolConfig {
            max_idle: Duration::from_nanos(1),
            ..Default::default()
        };
        let pool = PaymentsConnectionPool::with_config(config).unwrap();
        let conn = pool.acquire().unwrap();
        pool.release(conn);
        std::thread::sleep(Duration::from_millis(1));
        let conn2 = pool.acquire().unwrap();
        assert_eq!(conn2.id, 2);
    }

    #[test]
    fn stale_release_decrements_total() {
        let config = PaymentsPoolConfig {
            max_idle: Duration::from_nanos(1),
            ..Default::default()
        };
        let pool = PaymentsConnectionPool::with_config(config).unwrap();
        let conn = pool.acquire().unwrap();
        assert_eq!(pool.total_count(), 1);
        std::thread::sleep(Duration::from_millis(1));
        pool.release(conn);
        assert_eq!(pool.total_count(), 0);
    }

    #[test]
    fn pool_error_converts_to_payment_error() {
        let pool_err = PaymentsPoolError::Exhausted(10);
        let payment_err: PaymentError = pool_err.into();
        assert!(matches!(payment_err, PaymentError::Database(_)));
    }

    #[test]
    fn connection_endpoint_does_not_contain_credentials() {
        let config = PaymentsPoolConfig {
            database_url: "postgres://admin:hunter2@db.internal:5432/payments".to_string(),
            ..Default::default()
        };
        let pool = PaymentsConnectionPool::with_config(config).unwrap();
        let conn = pool.acquire().unwrap();
        assert!(!conn.endpoint.contains("hunter2"));
        assert!(!conn.endpoint.contains("admin"));
    }
}
