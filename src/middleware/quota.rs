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
        let key = key.to_string();
        let client = self.redis_client.clone();
        let ttl: i64 = self
            .cb
            .call(|| async move {
                let mut conn = client.get_multiplexed_async_connection().await?;
                conn.ttl(&key).await
            })
            .await
            .map_err(redis_cb_err)?;
        Ok(if ttl < 0 { 0 } else { ttl as u64 })
    }

    /// Consume one request against a fixed per-window limit (window_secs TTL).
    /// Returns `true` if the request is allowed, `false` if the limit is exceeded.
    pub async fn consume_quota_with_window(
        &self,
        key: &str,
        limit: u32,
        window_secs: i64,
    ) -> Result<bool, redis::RedisError> {
        let usage_key = format!("quota:usage:{key}");
        let client = self.redis_client.clone();
        let usage_key2 = usage_key.clone();

        let current: u32 = self
            .cb
            .call(|| async move {
                let mut conn = client.get_multiplexed_async_connection().await?;
                let current: u32 = conn.incr(&usage_key2, 1).await?;
                if current == 1 {
                    let _: () = conn.expire(&usage_key2, window_secs).await?;
                }
                Ok(current)
            })
            .await
            .map_err(redis_cb_err)?;

        Ok(current <= limit)
    }

    /// Check quota status against an explicit limit (does not consume).
    pub async fn check_quota_with_limit(
        &self,
        key: &str,
        limit: u32,
    ) -> Result<QuotaStatus, redis::RedisError> {
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

// ---------------------------------------------------------------------------
// Axum middleware
// ---------------------------------------------------------------------------

use axum::{
    body::Body,
    extract::State,
    http::{HeaderValue, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::AppState;

const DEFAULT_RATE_LIMIT_PER_MINUTE: u32 = 100;

/// Per-tenant rate limiting middleware.
///
/// - Identifies the tenant via `X-API-Key` or `X-Tenant-ID` header.
/// - Uses `tenants.rate_limit_per_minute` when available; falls back to 100 req/min.
/// - Unauthenticated requests share a single `anon` bucket capped at 100 req/min.
/// - Returns `429 Too Many Requests` with `Retry-After`, `X-RateLimit-Limit`,
///   `X-RateLimit-Remaining`, and `X-RateLimit-Reset` headers on exhaustion.
pub async fn rate_limit_middleware(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next<Body>,
) -> Response {
    // Derive a quota key: prefer API key, then tenant-id header, then "anon".
    let quota_key = req
        .headers()
        .get("x-api-key")
        .or_else(|| req.headers().get("X-API-Key"))
        .and_then(|v| v.to_str().ok())
        .map(|k| k.to_string())
        .or_else(|| {
            req.headers()
                .get("X-Tenant-ID")
                .and_then(|v| v.to_str().ok())
                .map(|id| format!("tenant:{id}"))
        })
        .unwrap_or_else(|| "anon".to_string());

    // Look up per-tenant limit from the in-memory tenant config cache.
    let limit_per_minute: u32 = {
        let configs = state.tenant_configs.read().await;
        // Try to match by API key (stored as the quota_key itself) or tenant UUID.
        configs
            .values()
            .find(|c| {
                quota_key == c.tenant_id.to_string()
                    || quota_key.starts_with("tenant:")
                        && quota_key.trim_start_matches("tenant:") == c.tenant_id.to_string()
            })
            .map(|c| c.rate_limit_per_minute as u32)
            .unwrap_or(DEFAULT_RATE_LIMIT_PER_MINUTE)
    };

    // Build a QuotaManager backed by the app's Redis URL.
    let manager = match QuotaManager::new(&state.redis_url) {
        Ok(m) => m,
        Err(_) => {
            // Redis unavailable — fail open to avoid blocking all traffic.
            tracing::warn!("rate_limit: Redis unavailable, skipping quota check");
            return next.run(req).await;
        }
    };

    // Override the quota config with the per-tenant per-minute limit.
    let per_minute_key = format!("ratelimit:{quota_key}");
    let quota_cfg = Quota {
        tier: Tier::Free,
        custom_limit: Some(limit_per_minute),
        reset_schedule: ResetSchedule::Hourly, // TTL managed manually below
    };
    // Best-effort: set config (ignore errors).
    let _ = manager.set_quota_config(&per_minute_key, &quota_cfg).await;

    // Consume one unit.
    let allowed = manager
        .consume_quota_with_window(&per_minute_key, limit_per_minute, 60)
        .await
        .unwrap_or(true); // fail open on Redis error

    // Read back status for headers.
    let status = manager
        .check_quota_with_limit(&per_minute_key, limit_per_minute)
        .await
        .unwrap_or(QuotaStatus {
            limit: limit_per_minute,
            used: 0,
            remaining: limit_per_minute,
            reset_in_seconds: 60,
        });

    if !allowed {
        let retry_after = status.reset_in_seconds.max(1).to_string();
        let mut response = (StatusCode::TOO_MANY_REQUESTS, "Too Many Requests").into_response();
        let headers = response.headers_mut();
        headers.insert(
            "X-RateLimit-Limit",
            HeaderValue::from_str(&status.limit.to_string()).unwrap(),
        );
        headers.insert(
            "X-RateLimit-Remaining",
            HeaderValue::from_str(&status.remaining.to_string()).unwrap(),
        );
        headers.insert(
            "X-RateLimit-Reset",
            HeaderValue::from_str(&status.reset_in_seconds.to_string()).unwrap(),
        );
        headers.insert("Retry-After", HeaderValue::from_str(&retry_after).unwrap());
        return response;
    }

    let mut response = next.run(req).await;
    let headers = response.headers_mut();
    headers.insert(
        "X-RateLimit-Limit",
        HeaderValue::from_str(&status.limit.to_string()).unwrap(),
    );
    headers.insert(
        "X-RateLimit-Remaining",
        HeaderValue::from_str(&status.remaining.to_string()).unwrap(),
    );
    headers.insert(
        "X-RateLimit-Reset",
        HeaderValue::from_str(&status.reset_in_seconds.to_string()).unwrap(),
    );
    response
}
