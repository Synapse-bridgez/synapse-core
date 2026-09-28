use std::sync::Arc;
use std::time::Duration;

use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::cache::error_handling::{record_degraded_mode, DegradedComponent};

/// Query cache backed by Redis.
///
/// ## Graceful degradation
///
/// Redis is treated as a best-effort accelerator, never as a source of truth.
/// When Redis is unavailable every operation degrades to a direct database
/// read (cache miss) instead of hard-failing the request. The degraded path
/// emits the shared degraded-mode signal so operators can observe the blast
/// radius of a Redis outage.
pub struct QueryCache {
    client: redis::Client,
    ttl: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedQuery {
    pub key: String,
    pub value: String,
}

impl QueryCache {
    pub fn new(client: redis::Client, ttl: Duration) -> Self {
        Self { client, ttl }
    }

    /// Attempt to fetch a cached query result.
    ///
    /// On any Redis error this returns `Ok(None)` (a cache miss) so the caller
    /// falls through to a direct DB read, and records the degraded-mode signal.
    pub async fn get(&self, key: &str) -> Option<CachedQuery> {
        let mut conn = match self.client.get_async_connection().await {
            Ok(conn) => conn,
            Err(err) => {
                self.record_degraded("get", &err);
                return None;
            }
        };

        match conn.get::<_, Option<String>>(key).await {
            Ok(Some(raw)) => match serde_json::from_str::<CachedQuery>(&raw) {
                Ok(cached) => Some(cached),
                Err(err) => {
                    warn!(key = %key, error = %err, "query cache entry failed to deserialize; treating as miss");
                    None
                }
            },
            Ok(None) => None,
            Err(err) => {
                self.record_degraded("get", &err);
                None
            }
        }
    }

    /// Store a query result in the cache.
    ///
    /// A failed write is non-fatal: the value was already computed from the DB,
    /// so we simply skip caching and record the degraded-mode signal.
    pub async fn set(&self, cached: &CachedQuery) {
        let mut conn = match self.client.get_async_connection().await {
            Ok(conn) => conn,
            Err(err) => {
                self.record_degraded("set", &err);
                return;
            }
        };

        let raw = match serde_json::to_string(cached) {
            Ok(raw) => raw,
            Err(err) => {
                warn!(key = %cached.key, error = %err, "query cache entry failed to serialize; skipping cache write");
                return;
            }
        };

        let ttl_secs = self.ttl.as_secs().max(1);
        if let Err(err) = conn
            .set_ex::<_, _, ()>(&cached.key, raw, ttl_secs)
            .await
        {
            self.record_degraded("set", &err);
        }
    }

    /// Invalidate a cached query.
    ///
    /// Invalidation failures are non-fatal; the entry will expire via TTL.
    pub async fn invalidate(&self, key: &str) {
        let mut conn = match self.client.get_async_connection().await {
            Ok(conn) => conn,
            Err(err) => {
                self.record_degraded("invalidate", &err);
                return;
            }
        };

        if let Err(err) = conn.del::<_, ()>(key).await {
            self.record_degraded("invalidate", &err);
        }
    }

    /// Emit the shared degraded-mode signal for a Redis failure.
    fn record_degraded(&self, operation: &str, err: &redis::RedisError) {
        debug!(operation = %operation, error = %err, "query cache degraded to direct DB read");
        record_degraded_mode(DegradedComponent::QueryCache, operation, err);
    }
}

/// Convenience wrapper used by callers that want a cache-or-DB read.
///
/// The `db_fetch` closure is always invoked when the cache is unavailable,
/// guaranteeing a correct (if slower) result during a Redis outage.
pub async fn get_or_fetch<F, Fut, T>(
    cache: Arc<QueryCache>,
    key: &str,
    db_fetch: F,
) -> T
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = T>,
{
    if let Some(cached) = cache.get(key).await {
        if let Ok(value) = serde_json::from_str::<T>(&cached.value) {
            return value;
        }
    }

    let value = db_fetch().await;

    if let Ok(serialized) = serde_json::to_string(&value) {
        cache
            .set(&CachedQuery {
                key: key.to_string(),
                value: serialized,
            })
            .await;
    }

    value
}
