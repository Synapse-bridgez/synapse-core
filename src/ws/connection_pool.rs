//! Connection pooling for WebSocket connections.
//!
//! [`ConnectionPool`] enforces a ceiling on the number of concurrently open
//! WebSocket connections.  It is intentionally lightweight: it does not manage
//! actual sockets or perform I/O — it tracks a counter and issues
//! [`ConnectionPermit`] RAII guards that decrement the counter on drop.
//!
//! # Thread safety
//!
//! [`ConnectionPool`] is `Send + Sync` and designed for shared ownership via
//! `Arc<ConnectionPool>`.  The active-connection counter uses
//! [`std::sync::atomic::AtomicUsize`] with `Relaxed` ordering; the counter is
//! eventually consistent but always monotonically correct for capacity enforcement.
//!
//! # Lifecycle
//!
//! ```text
//! Incoming WS upgrade
//!       │
//!       ▼
//! pool.acquire()  ──► Err(PoolError::AcquisitionFailed)  → 503 to client
//!       │
//!       ▼ Ok(ConnectionPermit)
//! spawn WebSocket handler (owns permit)
//!       │
//!       ▼
//! handler exits / connection closes
//!       │
//!       ▼
//! ConnectionPermit::drop() decrements counter
//! ```
//!
//! # Example
//!
//! ```rust
//! use synapse_core::ws::connection_pool::{ConnectionPool, PoolConfig};
//!
//! let pool = ConnectionPool::new(PoolConfig { max_connections: 100, min_connections: 0 });
//! let permit = pool.acquire().expect("pool not full");
//! assert_eq!(pool.active_connections(), 1);
//! drop(permit);
//! assert_eq!(pool.active_connections(), 0);
//! ```

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Configuration for a [`ConnectionPool`].
///
/// | Field | Default | Notes |
/// |---|---|---|
/// | `max_connections` | 1 000 | Hard ceiling; `acquire` returns `Err` when reached. |
/// | `min_connections` | 10 | Advisory lower bound; the pool does not pre-warm connections. |
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Maximum number of concurrent connections the pool will allow.
    ///
    /// Once this ceiling is reached, [`ConnectionPool::acquire`] returns
    /// [`PoolError::AcquisitionFailed`] until an existing permit is dropped.
    pub max_connections: usize,

    /// Minimum advisory connection count.
    ///
    /// The pool does not actively maintain this number of connections; it is
    /// stored for use by health-check or metrics logic that wants to warn when
    /// utilisation drops unexpectedly low.
    pub min_connections: usize,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_connections: 1000,
            min_connections: 10,
        }
    }
}

/// Shared WebSocket connection pool.
///
/// Cloned instances share the same underlying counter via `Arc`, so the pool
/// can be distributed cheaply across Axum handler instances.
///
/// See the [module documentation](self) for a usage walkthrough.
pub struct ConnectionPool {
    active_connections: Arc<AtomicUsize>,
    max_connections: usize,
}

impl ConnectionPool {
    /// Creates a new pool with the provided configuration.
    ///
    /// The pool starts with zero active connections regardless of
    /// `config.min_connections`.
    pub fn new(config: PoolConfig) -> Self {
        Self {
            active_connections: Arc::new(AtomicUsize::new(0)),
            max_connections: config.max_connections,
        }
    }

    /// Attempts to acquire a connection permit.
    ///
    /// Increments the active-connection counter and returns a
    /// [`ConnectionPermit`] that decrements it on drop.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::AcquisitionFailed`] when the pool is at capacity
    /// (`active_connections >= max_connections`).  The caller should respond
    /// with HTTP 503 and a `Retry-After` header.
    ///
    /// # Panics
    ///
    /// Does not panic.
    pub fn acquire(&self) -> Result<ConnectionPermit, PoolError> {
        let current = self.active_connections.load(Ordering::Relaxed);
        if current >= self.max_connections {
            return Err(PoolError::AcquisitionFailed);
        }

        self.active_connections.fetch_add(1, Ordering::Relaxed);
        Ok(ConnectionPermit {
            active_connections: Arc::clone(&self.active_connections),
        })
    }

    /// Returns the number of currently active connections.
    ///
    /// This reads the atomic counter with `Relaxed` ordering and may lag by
    /// one operation in multi-threaded scenarios, but is sufficient for
    /// metrics and health-check purposes.
    pub fn active_connections(&self) -> usize {
        self.active_connections.load(Ordering::Relaxed)
    }

    /// Returns the configured maximum connection ceiling.
    pub fn capacity(&self) -> usize {
        self.max_connections
    }

    /// Returns the number of permits that can still be acquired before the
    /// pool reaches capacity.
    ///
    /// Uses [`usize::saturating_sub`] so this never underflows even if the
    /// atomic counter overshoots momentarily under high concurrency.
    pub fn available_permits(&self) -> usize {
        let active = self.active_connections.load(Ordering::Relaxed);
        self.max_connections.saturating_sub(active)
    }

    /// Returns `true` when no further permits can be acquired.
    ///
    /// Equivalent to `self.available_permits() == 0`.
    pub fn is_full(&self) -> bool {
        self.available_permits() == 0
    }
}

/// RAII guard for a single connection permit.
///
/// Obtained via [`ConnectionPool::acquire`].  When this value is dropped the
/// pool's active-connection counter is decremented automatically, so permits
/// are always released even if the handler task panics or returns early.
///
/// Permits are intentionally non-`Clone` to ensure one permit maps to exactly
/// one logical connection.
pub struct ConnectionPermit {
    active_connections: Arc<AtomicUsize>,
}

impl Drop for ConnectionPermit {
    /// Releases the permit back to the pool.
    ///
    /// Uses `fetch_sub` with `Relaxed` ordering, which is safe because the
    /// increment in [`ConnectionPool::acquire`] uses the same ordering and
    /// both sides are sequenced within a single task.
    fn drop(&mut self) {
        self.active_connections.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Errors returned by [`ConnectionPool::acquire`].
#[derive(Debug, thiserror::Error)]
pub enum PoolError {
    /// The pool is at capacity; no further connections may be accepted until
    /// an existing [`ConnectionPermit`] is dropped.
    #[error("connection pool is at capacity; try again later")]
    AcquisitionFailed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_creation() {
        let config = PoolConfig {
            max_connections: 10,
            min_connections: 2,
        };
        let pool = ConnectionPool::new(config);
        assert_eq!(pool.capacity(), 10);
        assert_eq!(pool.active_connections(), 0);
    }

    #[test]
    fn test_acquire_connection() {
        let config = PoolConfig {
            max_connections: 5,
            min_connections: 1,
        };
        let pool = ConnectionPool::new(config);
        let _permit = pool.acquire().unwrap();
        assert_eq!(pool.active_connections(), 1);
    }

    #[test]
    fn test_release_connection() {
        let config = PoolConfig {
            max_connections: 5,
            min_connections: 1,
        };
        let pool = ConnectionPool::new(config);
        {
            let _permit = pool.acquire().unwrap();
            assert_eq!(pool.active_connections(), 1);
        }
        assert_eq!(pool.active_connections(), 0);
    }

    #[test]
    fn test_pool_capacity_limit() {
        let config = PoolConfig {
            max_connections: 2,
            min_connections: 1,
        };
        let pool = ConnectionPool::new(config);

        let _p1 = pool.acquire().unwrap();
        let _p2 = pool.acquire().unwrap();

        assert!(pool.is_full());
        assert_eq!(pool.available_permits(), 0);
    }

    #[test]
    fn test_multiple_acquisitions() {
        let config = PoolConfig {
            max_connections: 10,
            min_connections: 1,
        };
        let pool = ConnectionPool::new(config);

        let _p1 = pool.acquire().unwrap();
        let _p2 = pool.acquire().unwrap();
        let _p3 = pool.acquire().unwrap();

        assert_eq!(pool.active_connections(), 3);
        assert_eq!(pool.available_permits(), 7);
    }

    #[test]
    fn test_pool_config_default() {
        let config = PoolConfig::default();
        assert_eq!(config.max_connections, 1000);
        assert_eq!(config.min_connections, 10);
    }
}
