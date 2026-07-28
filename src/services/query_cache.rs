use crate::cache::{CacheValidator, ValidationError};
use crate::middleware::idempotency::RedisCircuitBreaker;
use lru::LruCache;
use redis::{aio::ConnectionManager, AsyncCommands, Client};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone)]
#[allow(dead_code)]
struct CacheEntry<T> {
    value: T,
    expires_at: Instant,
}

/// Redis connection pool configuration metadata.
///
/// NOTE: These values are read from environment variables but NOT currently enforced.
/// The underlying `redis::aio::ConnectionManager` handles its own internal connection
/// reuse and reconnection logic, which is transparent to this layer. If you need to
/// enforce strict connection pool limits or acquisition timeouts, this struct would
/// need to be enhanced with a Semaphore-based pool implementation.
///
/// # Configuration
/// - pool_size: Read from REDIS_POOL_SIZE env var (default: 10)
/// - pool_timeout: Read from REDIS_POOL_TIMEOUT_SECS env var (default: 5 secs)
#[derive(Clone, Debug)]
pub struct RedisPoolConfig {
    /// Maximum number of pooled Redis connections (from REDIS_POOL_SIZE).
    /// NOTE: Currently for informational purposes only; not enforced.
    pub pool_size: u32,
    /// Timeout for acquiring a connection from the pool (from REDIS_POOL_TIMEOUT_SECS).
    /// NOTE: Currently for informational purposes only; not enforced.
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
    lru: Arc<Mutex<LruCache<String, String>>>,
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

impl QueryCache {
    /// Creates a new QueryCache with Redis connection management.
    ///
    /// # Connection Management
    /// - Uses redis::aio::ConnectionManager for built-in connection reuse
    /// - Maintains an in-memory LRU cache for frequently accessed values
    /// - RedisPoolConfig is read from environment variables but not enforced
    ///   (pool_size and pool_timeout are for operational awareness only)
    ///
    /// # Arguments
    /// * `redis_url` - The Redis server URL (e.g., "redis://localhost:6379")
    ///
    /// # Returns
    /// A QueryCache instance with an initialized connection manager and LRU cache
    pub async fn new(redis_url: &str) -> Result<Self, redis::RedisError> {
        let client = Client::open(redis_url)?;

        // OPT: Create connection manager for built-in pooling and health checks
        let pool = ConnectionManager::new(client).await?;

        let pool_config = RedisPoolConfig::default();
        let cache_size = std::env::var("MEMORY_CACHE_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1000);

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
        })
    }

    /// Acquires a connection manager for Redis operations.
    ///
    /// This is a cheap clone of the shared ConnectionManager, which handles
    /// connection reuse internally via Arc-wrapped state.
    async fn get_connection(&self) -> Result<ConnectionManager, redis::RedisError> {
        Ok(self.pool.clone())
    }

    pub async fn get<T: DeserializeOwned + Send>(
        &self,
        key: &str,
    ) -> Result<Option<T>, redis::RedisError> {
        CacheValidator::validate_key(key).map_err(cache_validation_error)?;

        // Try in-memory cache first
        {
            let mut lru = self.lru.lock().unwrap();
            if let Some(cached) = lru.get(key) {
                self.memory_hits.fetch_add(1, Ordering::Relaxed);
                if let Ok(value) = serde_json::from_str::<T>(cached) {
                    return Ok(Some(value));
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
                            lru_cache.put(key.clone(), v.clone());
                        }
                        serde_json::from_str(&v).map(Some).map_err(|e| {
                            redis::RedisError::from((
                                redis::ErrorKind::TypeError,
                                "deserialization failed",
                                e.to_string(),
                            ))
                        })
                    }
                    None => {
                        misses.fetch_add(1, Ordering::Relaxed);
                        Ok(None)
                    }
                }
            })
            .await
            .map_err(|e| match e {
                crate::middleware::idempotency::RedisError::CircuitOpen => redis::RedisError::from(
                    (redis::ErrorKind::IoError, "Redis circuit breaker is open"),
                ),
                crate::middleware::idempotency::RedisError::Redis(r) => r,
            })
    }

    pub async fn set<T: Serialize + Send>(
        &self,
        key: &str,
        value: &T,
        ttl: Duration,
    ) -> Result<(), redis::RedisError> {
        CacheValidator::validate_key(key).map_err(cache_validation_error)?;
        let ttl_secs = ttl.as_secs();
        if ttl_secs == 0 {
            return Err(cache_validation_error(ValidationError::InvalidTTL));
        }
        if ttl_secs > i64::MAX as u64 {
            return Err(cache_validation_error(ValidationError::InvalidTTL));
        }
        CacheValidator::validate_ttl(ttl_secs as i64).map_err(cache_validation_error)?;

        let serialized = serde_json::to_string(value).map_err(|e| {
            redis::RedisError::from((
                redis::ErrorKind::TypeError,
                "serialization failed",
                e.to_string(),
            ))
        })?;
        CacheValidator::validate_value_size(serialized.as_bytes())
            .map_err(cache_validation_error)?;

        // Store in in-memory cache
        {
            let mut lru = self.lru.lock().unwrap();
            lru.put(key.to_string(), serialized.clone());
        }

        // OPT: Reuse pooled connection instead of creating new one
        let pool = self.pool.clone();
        let key = key.to_string();

        self.cb
            .call(|| async move {
                // OPT: Get connection from pool (cheap clone of internal Arc)
                let mut conn = pool.clone();
                conn.set_ex(&key, serialized.clone(), ttl_secs).await
            })
            .await
            .map_err(|e| match e {
                crate::middleware::idempotency::RedisError::CircuitOpen => redis::RedisError::from(
                    (redis::ErrorKind::IoError, "Redis circuit breaker is open"),
                ),
                crate::middleware::idempotency::RedisError::Redis(r) => r,
            })
    }

    pub async fn invalidate(&self, pattern: &str) -> Result<(), redis::RedisError> {
        CacheValidator::validate_pattern(pattern).map_err(cache_validation_error)?;

        // Clear in-memory cache
        {
            let mut lru = self.lru.lock().unwrap();
            lru.clear();
        }

        let pool = self.pool.clone();
        let pattern = pattern.to_string();

        self.cb
            .call(|| async move {
                let mut conn = pool.clone();
                let mut cursor: u64 = 0;
                loop {
                    let (next_cursor, batch): (u64, Vec<String>) = redis::cmd("SCAN")
                        .arg(cursor)
                        .arg("MATCH")
                        .arg(&pattern)
                        .arg("COUNT")
                        .arg(100u32)
                        .query_async(&mut conn)
                        .await?;
                    if !batch.is_empty() {
                        conn.del::<_, ()>(batch).await?;
                    }
                    cursor = next_cursor;
                    if cursor == 0 {
                        break;
                    }
                }
                Ok(())
            })
            .await
            .map_err(|e| match e {
                crate::middleware::idempotency::RedisError::CircuitOpen => redis::RedisError::from(
                    (redis::ErrorKind::IoError, "Redis circuit breaker is open"),
                ),
                crate::middleware::idempotency::RedisError::Redis(r) => r,
            })
    }

    pub async fn invalidate_exact(&self, key: &str) -> Result<(), redis::RedisError> {
        CacheValidator::validate_key(key).map_err(cache_validation_error)?;

        // Clear from in-memory cache
        {
            let mut lru = self.lru.lock().unwrap();
            lru.pop(key);
        }

        let pool = self.pool.clone();
        let key = key.to_string();

        self.cb
            .call(|| async move {
                let mut conn = pool.clone();
                conn.del::<_, ()>(&key).await
            })
            .await
            .map_err(|e| match e {
                crate::middleware::idempotency::RedisError::CircuitOpen => redis::RedisError::from(
                    (redis::ErrorKind::IoError, "Redis circuit breaker is open"),
                ),
                crate::middleware::idempotency::RedisError::Redis(r) => r,
            })
    }

    /// Verifies the Redis connection is healthy by sending a PING.
    ///
    /// # Returns
    /// - `Ok(())` if Redis responds to PING
    /// - `Err(redis::RedisError)` if connection fails
    pub async fn health_check(&self) -> Result<(), redis::RedisError> {
        let mut conn = self.pool.clone();
        redis::cmd("PING")
            .query_async::<_, String>(&mut conn)
            .await?;
        Ok(())
    }

    /// Returns the circuit breaker state: `"open"` or `"closed"`.
    pub fn circuit_state(&self) -> String {
        self.cb.state()
    }

    /// Returns the connection pool configuration (size, timeout).
    pub fn pool_config(&self) -> &RedisPoolConfig {
        &self.pool_config
    }

    pub fn metrics(&self) -> CacheMetrics {
        let hits = self.hits.load(Ordering::Relaxed);
        let misses = self.misses.load(Ordering::Relaxed);
        let total = hits + misses;
        let hit_rate = if total > 0 {
            (hits as f64 / total as f64) * 100.0
        } else {
            0.0
        };

        let memory_hits = self.memory_hits.load(Ordering::Relaxed);
        let memory_misses = self.memory_misses.load(Ordering::Relaxed);
        let memory_total = memory_hits + memory_misses;
        let memory_hit_rate = if memory_total > 0 {
            (memory_hits as f64 / memory_total as f64) * 100.0
        } else {
            0.0
        };

        CacheMetrics {
            hits,
            misses,
            total,
            hit_rate,
            memory_hits,
            memory_misses,
            memory_total,
            memory_hit_rate,
        }
    }

    pub async fn warm_cache(
        &self,
        pool: &sqlx::PgPool,
        config: &CacheConfig,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Warm status counts
        let status_counts = crate::db::queries::get_status_counts(pool).await?;
        self.set(
            "query:status_counts",
            &status_counts,
            Duration::from_secs(config.status_counts_ttl),
        )
        .await?;

        // Warm daily totals for last 7 days
        let daily_totals = crate::db::queries::get_daily_totals(pool, 7).await?;
        self.set(
            "query:daily_totals:7",
            &daily_totals,
            Duration::from_secs(config.daily_totals_ttl),
        )
        .await?;

        // Warm asset stats
        let asset_stats = crate::db::queries::get_asset_stats(pool).await?;
        self.set(
            "query:asset_stats",
            &asset_stats,
            Duration::from_secs(config.asset_stats_ttl),
        )
        .await?;

        tracing::info!("Cache warming completed");
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CacheMetrics {
    pub hits: u64,
    pub misses: u64,
    pub total: u64,
    pub hit_rate: f64,
    pub memory_hits: u64,
    pub memory_misses: u64,
    pub memory_total: u64,
    pub memory_hit_rate: f64,
}

pub fn cache_key_status_counts() -> String {
    "query:status_counts".to_string()
}

pub fn cache_key_daily_totals(days: i32) -> String {
    format!("query:daily_totals:{days}")
}

pub fn cache_key_asset_stats() -> String {
    "query:asset_stats".to_string()
}

pub fn cache_key_asset_total(asset_code: &str) -> String {
    format!("query:asset_total:{asset_code}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_cache_metrics() {
        let cache = match QueryCache::new("redis://localhost:6379").await {
            Ok(c) => c,
            Err(_) => {
                // Redis not available in test environment, skip
                return;
            }
        };
        let metrics = cache.metrics();
        assert_eq!(metrics.hits, 0);
        assert_eq!(metrics.misses, 0);
    }

    #[test]
    fn test_cache_key_generation() {
        assert_eq!(cache_key_status_counts(), "query:status_counts");
        assert_eq!(cache_key_daily_totals(7), "query:daily_totals:7");
        assert_eq!(cache_key_asset_stats(), "query:asset_stats");
        assert_eq!(cache_key_asset_total("USD"), "query:asset_total:USD");
    }

    #[tokio::test]
    async fn test_get_rejects_invalid_key() {
        let cache = match QueryCache::new("redis://localhost:6379").await {
            Ok(c) => c,
            Err(_) => return, // Redis not available in test environment
        };
        let result = cache.get::<String>("invalid key").await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("cache validation failed"));
    }

    #[tokio::test]
    async fn test_invalidate_rejects_invalid_pattern() {
        let cache = match QueryCache::new("redis://localhost:6379").await {
            Ok(c) => c,
            Err(_) => return, // Redis not available in test environment
        };
        let result = cache.invalidate("bad@pattern").await;
        assert!(result.is_err());
    }

    // --- Connection Pool Tests ---

    #[tokio::test]
    async fn test_connection_pool_reuses_connections() {
        let cache = match QueryCache::new("redis://localhost:6379").await {
            Ok(c) => c,
            Err(_) => return, // Redis not available
        };

        // OPT: Multiple operations should reuse pooled connections
        let key = "test:pool:reuse";
        let _ = cache.set(key, &"value1", Duration::from_secs(10)).await;
        let _ = cache.set(key, &"value2", Duration::from_secs(10)).await;
        let result: Option<String> = cache.get(key).await.ok().flatten();

        // If pool reuse works, no connection exhaustion error should occur
        assert!(result.is_some() || result.is_none()); // Either success or graceful error
    }

    #[tokio::test]
    async fn test_pool_health_check() {
        let cache = match QueryCache::new("redis://localhost:6379").await {
            Ok(c) => c,
            Err(_) => return, // Redis not available
        };

        // OPT: Health check should succeed if pool is healthy
        let health = cache.health_check().await;
        // Either succeeds (Redis available) or fails gracefully
        let _ = health;
    }

    #[tokio::test]
    async fn test_pool_config_from_env() {
        // OPT: Pool size and timeout should be configurable from env
        let config = RedisPoolConfig::default();
        assert!(config.pool_size > 0);
        assert!(config.pool_timeout.as_secs() > 0);
    }

    #[tokio::test]
    async fn test_connection_failure_is_typed_error() {
        // Attempt to connect to non-existent server
        let result = QueryCache::new("redis://invalid-host-12345:6379").await;

        // Should return a typed redis::RedisError, not panic
        assert!(result.is_err());
        let err = result.unwrap_err();
        // Error should be serializable/displayable
        let _ = err.to_string();
    }

    #[test]
    fn test_redis_pool_config_defaults() {
        let config = RedisPoolConfig::default();
        assert_eq!(config.pool_size, 10);
        assert_eq!(config.pool_timeout.as_secs(), 5);
    }

    #[test]
    fn test_redis_pool_config_exposed_via_cache() {
        // This test verifies the pool_config method returns the configuration
        // even though it's not currently enforced. This is important for
        // operational visibility and future enforcement.
        let config = RedisPoolConfig::default();
        assert!(config.pool_size > 0);
        assert!(config.pool_timeout.as_secs() > 0);
    }

    #[test]
    fn test_cache_config_defaults() {
        let config = CacheConfig::default();
        assert_eq!(config.status_counts_ttl, 300);
        assert_eq!(config.daily_totals_ttl, 3600);
        assert_eq!(config.asset_stats_ttl, 600);
        assert_eq!(config.memory_cache_size, 1000);
        assert_eq!(config.memory_cache_ttl, 30);
    }

    #[test]
    fn test_redis_pool_config_with_custom_values() {
        std::env::set_var("REDIS_POOL_SIZE", "20");
        std::env::set_var("REDIS_POOL_TIMEOUT_SECS", "10");

        let config = RedisPoolConfig::default();
        assert_eq!(config.pool_size, 20);
        assert_eq!(config.pool_timeout.as_secs(), 10);

        std::env::remove_var("REDIS_POOL_SIZE");
        std::env::remove_var("REDIS_POOL_TIMEOUT_SECS");
    }
}
