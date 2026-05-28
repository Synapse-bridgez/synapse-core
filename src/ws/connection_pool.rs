/// Connection pooling for WebSocket connections
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Configuration for connection pool
#[derive(Debug, Clone)]
pub struct PoolConfig {
    pub max_connections: usize,
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

/// Manages WebSocket connection pooling
pub struct ConnectionPool {
    active_connections: Arc<AtomicUsize>,
    max_connections: usize,
}

impl ConnectionPool {
    /// Create a new connection pool
    pub fn new(config: PoolConfig) -> Self {
        Self {
            active_connections: Arc::new(AtomicUsize::new(0)),
            max_connections: config.max_connections,
        }
    }

    /// Try to acquire a connection permit
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

    /// Get number of active connections
    pub fn active_connections(&self) -> usize {
        self.active_connections.load(Ordering::Relaxed)
    }

    /// Get pool capacity
    pub fn capacity(&self) -> usize {
        self.max_connections
    }

    /// Get available permits
    pub fn available_permits(&self) -> usize {
        let active = self.active_connections.load(Ordering::Relaxed);
        self.max_connections.saturating_sub(active)
    }

    /// Check if pool is at capacity
    pub fn is_full(&self) -> bool {
        self.available_permits() == 0
    }
}

/// Guard for a connection permit
pub struct ConnectionPermit {
    active_connections: Arc<AtomicUsize>,
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.active_connections.fetch_sub(1, Ordering::Relaxed);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PoolError {
    #[error("Failed to acquire connection from pool")]
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
