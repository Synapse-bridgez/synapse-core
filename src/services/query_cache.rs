use crate::middleware::idempotency::RedisCircuitBreaker;
use redis::{aio::MultiplexedConnection, AsyncCommands, Client};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
pub struct QueryCache {
    client: Client,
    cb: RedisCircuitBreaker,
    hits: Arc<AtomicU64>,
    misses: Arc<AtomicU64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
    pub status_counts_ttl: u64,
    pub daily_totals_ttl: u64,
    pub asset_stats_ttl: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            status_counts_ttl: 300, // 5 minutes
            daily_totals_ttl: 3600, // 1 hour
            asset_stats_ttl: 600,   // 10 minutes
        }
    }
}

impl QueryCache {
    pub fn new(redis_url: &str) -> Result<Self, redis::RedisError> {
        let client = Client::open(redis_url)?;
        Ok(Self {
            client,
            cb: RedisCircuitBreaker::from_env(),
            hits: Arc::new(AtomicU64::new(0)),
            misses: Arc::new(AtomicU64::new(0)),
        })
    }

    async fn get_connection(&self) -> Result<MultiplexedConnection, redis::RedisError> {
        self.client.get_multiplexed_async_connection().await
    }

    pub async fn get<T: DeserializeOwned + Send>(
        &self,
        key: &str,
    ) -> Result<Option<T>, redis::RedisError> {
        let client = self.client.clone();
        let key = key.to_string();
        let hits = self.hits.clone();
        let misses = self.misses.clone();

        self.cb
            .call(|| async move {
                let mut conn = client.get_multiplexed_async_connection().await?;
                let value: Option<String> = conn.get(&key).await?;
                match value {
                    Some(v) => {
                        hits.fetch_add(1, Ordering::Relaxed);
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
        let serialized = serde_json::to_string(value).map_err(|e| {
            redis::RedisError::from((
                redis::ErrorKind::TypeError,
                "serialization failed",
                e.to_string(),
            ))
        })?;
        let client = self.client.clone();
        let key = key.to_string();

        self.cb
            .call(|| async move {
                let mut conn = client.get_multiplexed_async_connection().await?;
                conn.set_ex(&key, serialized, ttl.as_secs()).await
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
        let mut conn: MultiplexedConnection = self.get_connection().await?;
        let keys: Vec<String> = conn.keys(pattern).await?;

        if !keys.is_empty() {
            conn.del::<_, ()>(keys).await?;
        }
        Ok(())
    }

    pub async fn invalidate_exact(&self, key: &str) -> Result<(), redis::RedisError> {
        let mut conn: MultiplexedConnection = self.get_connection().await?;
        conn.del::<_, ()>(key).await
    }

    /// Returns the circuit breaker state: `"open"` or `"closed"`.
    pub fn circuit_state(&self) -> String {
        self.cb.state()
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

        CacheMetrics {
            hits,
            misses,
            total,
            hit_rate,
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
}

pub fn cache_key_status_counts() -> String {
    "query:status_counts".to_string()
}

pub fn cache_key_daily_totals(days: i32) -> String {
    format!("query:daily_totals:{}", days)
}

pub fn cache_key_asset_stats() -> String {
    "query:asset_stats".to_string()
}

pub fn cache_key_asset_total(asset_code: &str) -> String {
    format!("query:asset_total:{}", asset_code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_cache_metrics() {
        let cache = QueryCache::new("redis://localhost:6379").unwrap();
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
}
