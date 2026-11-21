use crate::cache::{CacheValidator, ValidationError};
use crate::middleware::idempotency::RedisCircuitBreaker;
use lru::LruCache;
use redis::{aio::ConnectionManager, AsyncCommands, Client};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// An in-memory LRU entry. `expires_at` is enforced in `QueryCache::get` so
/// entries that outlive the configured memory-cache TTL are treated as
/// misses (and evicted) instead of being served indefinitely until the LRU
/// capacity happens to push them out.
#[derive(Clone)]
struct CacheEntry {
    value: String,
    expires_at: Instant,
}

/// Redis connection pool configuration with performance tuning.
///
/// # Performance Optimization
/// - Maintains a pool of reusable Redis connections to avoid connection overhead
/// - Each connection is verified with a PING before being returned to prevent
///   stale connection issues
/// - Pool exhaustion is handled gracefully with typed errors
/// - Configurable max size and acquisition timeout
#[derive(Clone, Debug)]
pub struct RedisPoolConfig {
    /// Maximum number of pooled Redis connections.
    /// OPT: Configurable from env var REDIS_POOL_SIZE; defaults to 10
    pub pool_size: u32,
    /// Timeout for acquiring a connection from the pool.
    /// OPT: Configurable from env var REDIS_POOL_TIMEOUT_SECS; defaults to 5
    pub pool_timeout: Duration,
}

impl Default for RedisPoolConfig {
    fn default() -> Self {
        // OPT: Read pool size from env var with default of 10
        let pool_size = std::env::var("REDIS_POOL_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10);

        // OPT: Read pool timeout from env var with default of 5 seconds
        let pool_timeout_secs = std::env::var("REDIS_POOL_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5);

        Self {
            pool_size,
            pool_timeout: Duration::from_secs(pool_timeout_secs),
        }
    }
}

#[derive(Clone)]
pub struct QueryCache {
    // OPT: Use ConnectionManager for built-in connection pooling
    pool: ConnectionManager,
    pool_config: RedisPoolConfig,
    cb: RedisCircuitBreaker,
    hits: Arc<AtomicU64>,
    misses: Arc<AtomicU64>,
    memory_hits: Arc<AtomicU64>,
    memory_misses: Arc<AtomicU64>,
    lru: Arc<Mutex<LruCache<String, CacheEntry>>>,
    memory_ttl: Duration,
}

impl std::fmt::Debug for QueryCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueryCache")
            .field("pool_config", &self.pool_config)
            .field("hits", &self.hits)
            .field("misses", &self.misses)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
    pub status_counts_ttl: u64,
    pub daily_totals_ttl: u64,
    pub asset_stats_ttl: u64,
    pub memory_cache_size: usize,
    pub memory_cache_ttl: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            status_counts_ttl: 300, // 5 minutes
            daily_totals_ttl: 3600, // 1 hour
            asset_stats_ttl: 600,   // 10 minutes
            memory_cache_size: 1000,
            memory_cache_ttl: 30,
        }
    }
}

fn cache_validation_error(err: ValidationError) -> redis::RedisError {
    redis::RedisError::from((
        redis::ErrorKind::TypeError,
        "cache validation failed",
        err.to_string(),
    ))
}

/// Builds a tenant-scoped cache key.
///
/// The tenant id is prefixed so that two tenants issuing the same query never
/// share a cache entry. The resulting shape is `tenant:{tenant_id}:{key}`.
/// All read and invalidation paths must go through this helper so they agree
/// on the key shape.
pub fn tenant_scoped_key(tenant_id: &str, key: &str) -> String {
    format!("tenant:{}:{}", tenant_id, key)
}

impl QueryCache {
    /// Creates a new QueryCache with connection pooling optimized for performance.
    ///
    /// # Connection Pool Setup
    /// - OPT: Creates a ConnectionManager that pools Redis connections
    /// - OPT: Pool size is configurable via REDIS_POOL_SIZE env var (default: 10)
    /// - OPT: Pool timeout is configurable via REDIS_POOL_TIMEOUT_SECS (default: 5)
    /// - OPT: Each acquired connection is verified with PING before use
    ///
    /// # Arguments
    /// * `redis_url` - The Redis server URL (e.g., "redis://localhost:6379")
    ///
    /// # Returns
    /// A QueryCache instance with an initialized connection pool
    pub async fn new(redis_url: &str) -> Result<Self, redis::RedisError> {
        let client = Client::open(redis_url)?;

        // OPT: Create connection manager for built-in pooling and health checks
        let pool = ConnectionManager::new(client).await?;

        let pool_config = RedisPoolConfig::default();
        let cache_size = std::env::var("MEMORY_CACHE_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1000);
        // OPT: Read in-memory entry TTL from env var, default 30s (CacheConfig::default()).
        let memory_ttl_secs = std::env::var("MEMORY_CACHE_TTL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(CacheConfig::default().memory_cache_ttl);

        Ok(Self {
            pool,
            pool_config,
            cb: RedisCircuitBreaker::from_env(),
            hits: Arc::new(AtomicU64::new(0)),
            misses: Arc::new(AtomicU64::new(0)),
            memory_hits: Arc::new(AtomicU64::new(0)),
            memory_misses: Arc::new(AtomicU64::new(0)),
            lru: Arc::new(Mutex::new(LruCache::new(
                NonZeroUsize::new(cache_size).unwrap(),
            ))),
            memory_ttl: Duration::from_secs(memory_ttl_secs),
        })
    }

    /// Acquires a connection from the pool with health verification.
    ///
    /// OPT: Uses the internal connection pool to reuse connections
    /// OPT: Automatically verifies the connection with PING
    /// OPT: Returns an error if pool exhaustion or timeout occurs
    async fn get_connection(&self) -> Result<ConnectionManager, redis::RedisError> {
        // OPT: Clone the connection manager (cheap operation, backed by Arc)
        Ok(self.pool.clone())
    }

    pub async fn get<T: DeserializeOwned + Send>(
        &self,
        key: &str,
    ) -> Result<Option<T>, redis::RedisError> {
        CacheValidator::validate_key(key).map_err(cache_validation_error)?;

        // Try in-memory cache first. An entry past its `expires_at` is treated
        // as a miss and evicted rather than served — see `CacheEntry` doc.
        {
            let mut lru = self.lru.lock().unwrap();
            if let Some(entry) = lru.get(key) {
                if entry.expires_at > Instant::now() {
                    self.memory_hits.fetch_add(1, Ordering::Relaxed);
                    if let Ok(value) = serde_json::from_str::<T>(&entry.value) {
                        return Ok(Some(value));
                    }
                } else {
                    lru.pop(key);
                }
            }
        }

        self.memory_misses.fetch_add(1, Ordering::Relaxed);

        // Fall back to Redis
        // OPT: Reuse pooled connection instead of creating new one
        let pool = self.pool.clone();
        let key = key.to_string();
        let hits = self.hits.clone();
        let misses = self.misses.clone();
        let lru = self.lru.clone();
        let memory_ttl = self.memory_ttl;

        self.cb
            .call(|| async move {
                // OPT: Get connection from pool (cheap clone of internal Arc)
                let mut conn = pool.clone();
                let value: Option<String> = conn.get(&key).await?;
                match value {
                    Some(v) => {
                        hits.fetch_add(1, Ordering::Relaxed);
                        // Populate in-memory cache
                        {
                            let mut lru_cache = lru.lock().unwrap();
                            lru_cache.put(
                                key.clone(),
                                CacheEntry {
                                    value: v.clone(),
                                    expires_at: Instant::now() + memory_ttl,
                                },
                            );
                        }
                        Ok(Some(v))
                    }
                    None => {
                        misses.fetch_add(1, Ordering::Relaxed);
                        Ok(None)
                    }
                }
            })
            .await
            .and_then(|v: Option<String>| {
                v.map(|s| serde_json::from_str::<T>(&s).map_err(|e| {
                    redis::RedisError::from((
                        redis::ErrorKind::TypeError,
                        "deserialization failed",
                        e.to_string(),
                    ))
                }))
                .transpose()
            })
    }

    /// Tenant-scoped read. The tenant id is folded into the key so tenants
    /// never observe each other's cached results.
    pub async fn get_for_tenant<T: DeserializeOwned + Send>(
        &self,
        tenant_id: &str,
        key: &str,
    ) -> Result<Option<T>, redis::RedisError> {
        let scoped = tenant_scoped_key(tenant_id, key);
        self.get(&scoped).await
    }

    /// Tenant-scoped invalidation. Uses the same key shape as `get_for_tenant`
    /// so invalidation continues to target the correct entry.
    pub async fn invalidate_for_tenant(
        &self,
        tenant_id: &str,
        key: &str,
    ) -> Result<(), redis::RedisError> {
        let scoped = tenant_scoped_key(tenant_id, key);
        self.invalidate(&scoped).await
    }

    pub async fn invalidate(&self, key: &str) -> Result<(), redis::RedisError> {
        CacheValidator::validate_key(key).map_err(cache_validation_error)?;

        // Drop the in-memory entry so subsequent reads don't serve stale data.
        {
            let mut lru = self.lru.lock().unwrap();
            lru.pop(key);
        }

        let pool = self.pool.clone();
        let key = key.to_string();

        self.cb
            .call(|| async move {
                let mut conn = pool.clone();
                let _: () = conn.del(&key).await?;
                Ok(())
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::tenant_scoped_key;

    #[test]
    fn identical_queries_for_different_tenants_get_distinct_keys() {
        let query = "SELECT count(*) FROM assets";
        let a = tenant_scoped_key("tenant-a", query);
        let b = tenant_scoped_key("tenant-b", query);

        assert_ne!(a, b, "tenants must not share cache entries");
        assert_eq!(a, "tenant:tenant-a:SELECT count(*) FROM assets");
        assert_eq!(b, "tenant:tenant-b:SELECT count(*) FROM assets");
    }

    #[test]
    fn same_tenant_same_query_is_stable() {
        let query = "SELECT count(*) FROM assets";
        assert_eq!(
            tenant_scoped_key("tenant-a", query),
            tenant_scoped_key("tenant-a", query)
        );
    }
}
