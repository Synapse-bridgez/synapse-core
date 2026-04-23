use crate::middleware::idempotency::RedisCircuitBreaker;
use redis::{AsyncCommands, Client};
use serde::{Deserialize, Serialize};

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
}

impl QuotaManager {
    pub fn new(redis_url: &str) -> Result<Self, redis::RedisError> {
        let redis_client = Client::open(redis_url)?;
        Ok(Self {
            redis_client,
            cb: RedisCircuitBreaker::from_env(),
        })
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

        let usage_key = format!("quota:usage:{}", key);
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

        let usage_key = format!("quota:usage:{}", key);
        let ttl = quota.reset_schedule.ttl_seconds() as i64;
        let client = self.redis_client.clone();
        let usage_key2 = usage_key.clone();

        let current: u32 = self
            .cb
            .call(|| async move {
                let mut conn = client.get_multiplexed_async_connection().await?;
                let current: u32 = conn.incr(&usage_key2, 1).await?;
                if current == 1 {
                    let _: () = conn.expire(&usage_key2, ttl).await?;
                }
                Ok(current)
            })
            .await
            .map_err(redis_cb_err)?;

        Ok(current <= limit)
    }

    pub async fn get_quota_config(&self, key: &str) -> Result<Quota, redis::RedisError> {
        let config_key = format!("quota:config:{}", key);
        let client = self.redis_client.clone();

        let config_json: Option<String> = self
            .cb
            .call(|| async move {
                let mut conn = client.get_multiplexed_async_connection().await?;
                Ok::<Option<String>, redis::RedisError>(conn.get(&config_key).await?)
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
            None => Ok(Quota {
                tier: Tier::Free,
                custom_limit: None,
                reset_schedule: ResetSchedule::Hourly,
            }),
        }
    }

    pub async fn set_quota_config(
        &self,
        key: &str,
        quota: &Quota,
    ) -> Result<(), redis::RedisError> {
        let config_key = format!("quota:config:{}", key);
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

    pub async fn reset_quota(&self, key: &str) -> Result<(), redis::RedisError> {
        let usage_key = format!("quota:usage:{}", key);
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
        let key = key.to_string();
        let client = self.redis_client.clone();
        let ttl: i64 = self
            .cb
            .call(|| async move {
                let mut conn = client.get_multiplexed_async_connection().await?;
                Ok::<i64, redis::RedisError>(conn.ttl(&key).await?)
            })
            .await
            .map_err(redis_cb_err)?;
        Ok(if ttl < 0 { 0 } else { ttl as u64 })
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct QuotaStatus {
    pub limit: u32,
    pub used: u32,
    pub remaining: u32,
    pub reset_in_seconds: u64,
}

// Helper to extract API key from request
pub fn extract_quota_key(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .or_else(|| {
            // Fallback to IP-based quota
            headers
                .get("x-forwarded-for")
                .and_then(|v| v.to_str().ok())
                .map(|s| format!("ip:{}", s.split(',').next().unwrap_or(s).trim()))
        })
}
