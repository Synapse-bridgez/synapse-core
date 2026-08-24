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
                increment_with_expiry(&mut conn, &usage_key2, window_secs).await
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
const LOCAL_FALLBACK_MAX_BUCKETS: usize = 10_000;
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug)]
struct LocalBucket {
    window_started: Instant,
    used: u32,
}

#[derive(Debug, Default)]
struct LocalFallbackLimiter {
    buckets: HashMap<String, LocalBucket>,
}

#[derive(Debug)]
struct LocalQuotaResult {
    allowed: bool,
    status: QuotaStatus,
}

impl LocalFallbackLimiter {
    fn consume(&mut self, key: &str, limit: u32, now: Instant) -> LocalQuotaResult {
        if !self.buckets.contains_key(key) && self.buckets.len() >= LOCAL_FALLBACK_MAX_BUCKETS {
            // Sweep only when capacity is needed, keeping the common request
            // path O(1) while still reclaiming expired tenant buckets.
            self.buckets.retain(|_, bucket| {
                now.saturating_duration_since(bucket.window_started) < RATE_LIMIT_WINDOW
            });
        }

        if !self.buckets.contains_key(key) && self.buckets.len() >= LOCAL_FALLBACK_MAX_BUCKETS {
            return LocalQuotaResult {
                allowed: false,
                status: QuotaStatus {
                    limit,
                    used: limit,
                    remaining: 0,
                    reset_in_seconds: RATE_LIMIT_WINDOW.as_secs(),
                },
            };
        }

        let bucket = self.buckets.entry(key.to_owned()).or_insert(LocalBucket {
            window_started: now,
            used: 0,
        });
        if now.saturating_duration_since(bucket.window_started) >= RATE_LIMIT_WINDOW {
            bucket.window_started = now;
            bucket.used = 0;
        }
        bucket.used = bucket.used.saturating_add(1);
        let elapsed = now.saturating_duration_since(bucket.window_started);
        let reset_in_seconds = RATE_LIMIT_WINDOW.saturating_sub(elapsed).as_secs().max(1);

        LocalQuotaResult {
            allowed: bucket.used <= limit,
            status: QuotaStatus {
                limit,
                used: bucket.used,
                remaining: limit.saturating_sub(bucket.used),
                reset_in_seconds,
            },
        }
    }
}

fn local_fallback() -> &'static Mutex<LocalFallbackLimiter> {
    static LIMITER: OnceLock<Mutex<LocalFallbackLimiter>> = OnceLock::new();
    LIMITER.get_or_init(|| Mutex::new(LocalFallbackLimiter::default()))
}

/// Produce exactly one namespaced bucket whether the identifier is raw or was
/// already namespaced by the tenant header path.
fn canonical_quota_key(identifier: &str) -> String {
    let identifier = identifier.trim_start_matches("tenant:");
    format!("tenant:{identifier}")
}

fn consume_local_fallback(key: &str, limit: u32) -> LocalQuotaResult {
    // Recovering from a poisoned mutex is safe here: all bucket updates happen
    // while holding the guard, so the map remains structurally valid.
    local_fallback()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .consume(key, limit, Instant::now())
}

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

    let per_minute_key = canonical_quota_key(&quota_key);

    // Overload policy: Redis is authoritative when available. If it cannot be
    // reached (including an open circuit), enforce the same fixed-window limit
    // in a bounded, process-local map. The cap prevents attacker-controlled
    // identifiers from growing memory without bound; new identifiers fail
    // closed once the cap is full. In a multi-instance deployment the outage
    // limit is per instance until Redis recovers.
    let redis_result = match QuotaManager::new(&state.redis_url) {
        Ok(manager) => manager
            .consume_quota_with_window(&per_minute_key, limit_per_minute, 60)
            .await
            .map(|allowed| (manager, allowed)),
        Err(error) => Err(error),
    };

    let (allowed, status) = match redis_result {
        Ok((manager, allowed)) => {
            let fallback_used = if allowed {
                1
            } else {
                limit_per_minute.saturating_add(1)
            };
            let status = manager
                .check_quota_with_limit(&per_minute_key, limit_per_minute)
                .await
                .unwrap_or(QuotaStatus {
                    limit: limit_per_minute,
                    used: fallback_used,
                    remaining: limit_per_minute.saturating_sub(fallback_used),
                    reset_in_seconds: RATE_LIMIT_WINDOW.as_secs(),
                });
            (allowed, status)
        }
        Err(error) => {
            tracing::warn!(%error, "rate_limit: Redis unavailable; using bounded local limiter");
            let result = consume_local_fallback(&per_minute_key, limit_per_minute);
            (result.allowed, result.status)
        }
    };

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_key_is_canonical_for_raw_and_namespaced_identifiers() {
        assert_eq!(canonical_quota_key("abc"), "tenant:abc");
        assert_eq!(canonical_quota_key("tenant:abc"), "tenant:abc");
        assert_eq!(canonical_quota_key("tenant:tenant:abc"), "tenant:abc");
    }

    #[test]
    fn redis_down_fallback_does_not_allow_unlimited_requests() {
        let mut limiter = LocalFallbackLimiter::default();
        let now = Instant::now();

        assert!(limiter.consume("tenant:a", 2, now).allowed);
        assert!(limiter.consume("tenant:a", 2, now).allowed);
        let rejected = limiter.consume("tenant:a", 2, now);
        assert!(!rejected.allowed);
        assert_eq!(rejected.status.remaining, 0);
    }

    #[test]
    fn local_fallback_starts_a_new_window_after_expiry() {
        let mut limiter = LocalFallbackLimiter::default();
        let now = Instant::now();

        assert!(limiter.consume("tenant:a", 1, now).allowed);
        assert!(!limiter.consume("tenant:a", 1, now).allowed);
        assert!(
            limiter
                .consume("tenant:a", 1, now + RATE_LIMIT_WINDOW)
                .allowed
        );
    }
}
