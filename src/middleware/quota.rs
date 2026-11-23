use crate::middleware::idempotency::RedisCircuitBreaker;
use redis::{AsyncCommands, Client};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

const INCREMENT_WITH_EXPIRY_SCRIPT: &str = r#"
local current = redis.call('INCR', KEYS[1])
-- The TTL check also repairs counters left without an expiry by older versions.
if current == 1 or redis.call('TTL', KEYS[1]) < 0 then
    redis.call('EXPIRE', KEYS[1], ARGV[1])
end
return current
"#;

async fn increment_with_expiry(
    conn: &mut redis::aio::MultiplexedConnection,
    key: &str,
    ttl_seconds: i64,
) -> redis::RedisResult<u32> {
    redis::Script::new(INCREMENT_WITH_EXPIRY_SCRIPT)
        .key(key)
        .arg(ttl_seconds)
        .invoke_async(conn)
        .await
}

fn redis_cb_err(e: crate::middleware::idempotency::RedisError) -> redis::RedisError {
    match e {
        crate::middleware::idempotency::RedisError::CircuitOpen => {
            redis::RedisError::from((redis::ErrorKind::IoError, "Redis circuit breaker is open"))
        }
        crate::middleware::idempotency::RedisError::Redis(r) => r,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Tier {
    Free,
    Standard,
    Premium,
}

impl Tier {
    pub fn requests_per_hour(&self) -> u32 {
        match self {
            Tier::Free => 100,
            Tier::Standard => 1000,
            Tier::Premium => 10000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Quota {
    pub tier: Tier,
    pub custom_limit: Option<u32>,
    pub reset_schedule: ResetSchedule,
}

impl Default for Quota {
    fn default() -> Self {
        Self {
            tier: Tier::Free,
            custom_limit: None,
            reset_schedule: ResetSchedule::Hourly,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResetSchedule {
    Hourly,
    Daily,
    Monthly,
}

impl ResetSchedule {
    pub fn ttl_seconds(&self) -> u64 {
        match self {
            ResetSchedule::Hourly => 3600,
            ResetSchedule::Daily => 86400,
            ResetSchedule::Monthly => 2592000,
        }
    }
}

#[derive(Clone)]
pub struct QuotaManager {
    redis_client: Client,
    cb: RedisCircuitBreaker,
    default_quota: Quota,
}

impl QuotaManager {
    pub fn new(redis_url: &str) -> Result<Self, redis::RedisError> {
        let redis_client = Client::open(redis_url)?;
        Ok(Self {
            redis_client,
            cb: RedisCircuitBreaker::from_env(),
            default_quota: Quota::default(),
        })
    }

    /// Overrides the quota applied to tenants that have no explicit quota row.
    pub fn with_default_quota(mut self, default_quota: Quota) -> Self {
        self.default_quota = default_quota;
        self
    }

    /// Returns the circuit breaker state: `"open"` or `"closed"`.
    pub fn circuit_state(&self) -> String {
        self.cb.state()
    }

    pub async fn check_quota(&self, key: &str) -> Result<QuotaStatus, redis::RedisError> {
        let quota = self.get_quota_config(key).await?;
        let limit = quota
            .custom_limit
            .unwrap_or_else(|| quota.tier.requests_per_hour());

        let usage_key = format!("quota:usage:{key}");
        let client = self.redis_client.clone();
        let usage_key2 = usage_key.clone();

        let current: u32 = self
            .cb
            .call(|| async move {
                let mut conn = client.get_multiplexed_async_connection().await?;
                Ok::<u32, redis::RedisError>(conn.get(&usage_key2).await.unwrap_or(0))
            })
            .await
            .map_err(redis_cb_err)?;

        let reset_in_seconds = self.get_ttl(&usage_key).await?;

        Ok(QuotaStatus {
            limit,
            used: current,
            remaining: limit.saturating_sub(current),
            reset_in_seconds,
        })
    }

    pub async fn consume_quota(&self, key: &str) -> Result<bool, redis::RedisError> {
        let quota = self.get_quota_config(key).await?;
        let limit = quota
            .custom_limit
            .unwrap_or_else(|| quota.tier.requests_per_hour());

        let usage_key = format!("quota:usage:{key}");
        let ttl = quota.reset_schedule.ttl_seconds() as i64;
        let client = self.redis_client.clone();
        let usage_key2 = usage_key.clone();

        let current: u32 = self
            .cb
            .call(|| async move {
                let mut conn = client.get_multiplexed_async_connection().await?;
                increment_with_expiry(&mut conn, &usage_key2, ttl).await
            })
            .await
            .map_err(redis_cb_err)?;

        Ok(current <= limit)
    }

    pub async fn get_quota_config(&self, key: &str) -> Result<Quota, redis::RedisError> {
        let config_key = format!("quota:config:{key}");
        let client = self.redis_client.clone();

        let config_json: Option<String> = self
            .cb
            .call(|| async move {
                let mut conn = client.get_multiplexed_async_connection().await?;
                conn.get(&config_key).await
            })
            .await
            .map_err(redis_cb_err)?;

        match config_json {
            Some(json) => serde_json::from_str(&json).map_err(|e| {
                redis::RedisError::from((
                    redis::ErrorKind::TypeError,
                    "deserialization failed",
                    e.to_string(),
                ))
            }),
            None => {
                // A tenant without an explicit quota row must still be rate
                // limited. Fall back to the configured default quota instead
                // of treating the tenant as unlimited.
                tracing::debug!(
                    tenant_id = %key,
                    "no quota row found for tenant; applying default quota"
                );
                Ok(self.default_quota.clone())
            }
        }
    }

    pub async fn set_quota_config(
        &self,
        key: &str,
        quota: &Quota,
    ) -> Result<(), redis::RedisError> {
        let config_key = format!("quota:config:{key}");
        let json = serde_json::to_string(quota).map_err(|e| {
            redis::RedisError::from((
                redis::ErrorKind::TypeError,
                "serialization failed",
                e.to_string(),
            ))
        })?;
        let client = self.redis_client.clone();

        self.cb
            .call(|| async move {
                let mut conn = client.get_multiplexed_async_connection().await?;
                conn.set(&config_key, json).await
            })
            .await
            .map_err(redis_cb_err)
    }

    /// Consistency guard for the quota-config split-brain this codebase used
    /// to have (see `handlers::admin::quota::set_tenant_quota`): after that
    /// fix, `tenants.rate_limit_per_minute` is the single source of truth
    /// for the limit `rate_limit_middleware` enforces, and nothing should
    /// write to `quota:config:*` anymore. Any keys still present here are
    /// either stale pre-fix data or a regression writing to dead config
    /// again — both worth surfacing rather than letting silently diverge a
    /// second time. One SCAN iteration is a bounded approximation, not a
    /// full-keyspace guarantee; that's an acceptable tradeoff for a
    /// low-frequency background alert rather than a correctness-critical read.
    pub async fn scan_stale_quota_configs(
        &self,
        limit: usize,
    ) -> Result<Vec<String>, redis::RedisError> {
        let client = self.redis_client.clone();
        self.cb
            .call(|| async move {
                let mut conn = client.get_multiplexed_async_connection().await?;
                let (_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                    .arg(0)
                    .arg("MATCH")
                    .arg("quota:config:*")
                    .arg("COUNT")
                    .arg(1000)
                    .query_async(&mut conn)
                    .await?;
                Ok(keys)
            })
            .await
            .map(|mut keys: Vec<String>| {
                keys.truncate(limit);
                keys
            })
            .map_err(redis_cb_err)
    }

    pub async fn reset_quota(&self, key: &str) -> Result<(), redis::RedisError> {
        let usage_key = format!("quota:usage:{key}");
        let client = self.redis_client.clone();
        self.cb
            .call(|| async move {
                let mut conn = client.get_multiplexed_async_connection().await?;
                conn.del(&usage_key).await
            })
            .await
            .map_err(redis_cb_err)
    }

    async fn get_ttl(&self, key: &str) -> Result<u64, redis::RedisError> {
        let client = self.redis_client.clone();
        let key = key.to_string();
        self.cb
            .call(|| async move {
                let mut conn = client.get_multiplexed_async_connection().await?;
                let ttl: i64 = conn.ttl(&key).await?;
                Ok::<u64, redis::RedisError>(if ttl < 0 { 0 } else { ttl as u64 })
            })
            .await
            .map_err(redis_cb_err)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaStatus {
    pub limit: u32,
    pub used: u32,
    pub remaining: u32,
    pub reset_in_seconds: u64,
}

/// Resolves the effective limit for a tenant given an optional quota row.
///
/// Returns the tenant's own limit when a row exists, otherwise the configured
/// default quota's limit. This is the single decision point the middleware
/// uses so a missing row can never bypass enforcement.
pub fn effective_limit(quota: Option<&Quota>, default_quota: &Quota) -> u32 {
    let quota = quota.unwrap_or(default_quota);
    quota
        .custom_limit
        .unwrap_or_else(|| quota.tier.requests_per_hour())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_quota() -> Quota {
        Quota {
            tier: Tier::Free,
            custom_limit: None,
            reset_schedule: ResetSchedule::Hourly,
        }
    }

    #[test]
    fn missing_row_is_limited_at_default_rate() {
        let default = default_quota();
        let limit = effective_limit(None, &default);
        assert_eq!(limit, Tier::Free.requests_per_hour());
        assert_eq!(limit, 100);
    }

    #[test]
    fn explicit_row_is_limited_at_its_own_rate() {
        let default = default_quota();
        let explicit = Quota {
            tier: Tier::Premium,
            custom_limit: None,
            reset_schedule: ResetSchedule::Hourly,
        };
        assert_eq!(
            effective_limit(Some(&explicit), &default),
            Tier::Premium.requests_per_hour()
        );

        let custom = Quota {
            tier: Tier::Standard,
            custom_limit: Some(42),
            reset_schedule: ResetSchedule::Daily,
        };
        assert_eq!(effective_limit(Some(&custom), &default), 42);
    }

    #[test]
    fn default_quota_override_is_respected_for_missing_row() {
        let default = Quota {
            tier: Tier::Standard,
            custom_limit: Some(7),
            reset_schedule: ResetSchedule::Hourly,
        };
        assert_eq!(effective_limit(None, &default), 7);
    }
}
