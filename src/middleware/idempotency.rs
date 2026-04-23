use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use failsafe::futures::CircuitBreaker as FuturesCircuitBreaker;
use failsafe::{backoff, failure_policy, Config, Error as FailsafeError, StateMachine};
use redis::Client;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

// ── Circuit breaker type alias ────────────────────────────────────────────────

type RedisCBInner =
    StateMachine<failure_policy::ConsecutiveFailures<backoff::EqualJittered>, ()>;

/// Shared Redis circuit breaker (cheaply cloneable).
#[derive(Clone)]
pub struct RedisCircuitBreaker {
    inner: RedisCBInner,
}

impl RedisCircuitBreaker {
    pub fn new(failure_threshold: u32, reset_timeout_secs: u64) -> Self {
        let backoff = backoff::equal_jittered(
            Duration::from_secs(reset_timeout_secs),
            Duration::from_secs(reset_timeout_secs * 2),
        );
        let policy = failure_policy::consecutive_failures(failure_threshold, backoff);
        Self {
            inner: Config::new().failure_policy(policy).build(),
        }
    }

    pub fn from_env() -> Self {
        let threshold = std::env::var("REDIS_CB_FAILURE_THRESHOLD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5u32);
        let timeout = std::env::var("REDIS_CB_RESET_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30u64);
        Self::new(threshold, timeout)
    }

    /// Returns `"open"` or `"closed"`.
    pub fn state(&self) -> String {
        if self.inner.is_call_permitted() {
            "closed".to_string()
        } else {
            "open".to_string()
        }
    }

    /// Execute `f` through the circuit breaker.
    pub async fn call<F, Fut, T>(&self, f: F) -> Result<T, RedisError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, redis::RedisError>>,
    {
        match self.inner.call(f()).await {
            Ok(v) => Ok(v),
            Err(FailsafeError::Rejected) => Err(RedisError::CircuitOpen),
            Err(FailsafeError::Inner(e)) => Err(RedisError::Redis(e)),
        }
    }
}

#[derive(Debug)]
pub enum RedisError {
    CircuitOpen,
    Redis(redis::RedisError),
}

impl std::fmt::Display for RedisError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RedisError::CircuitOpen => write!(f, "Redis circuit breaker is open"),
            RedisError::Redis(e) => write!(f, "Redis error: {e}"),
        }
    }
}

impl From<redis::RedisError> for RedisError {
    fn from(e: redis::RedisError) -> Self {
        RedisError::Redis(e)
    }
}

// ── IdempotencyService ────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct IdempotencyService {
    client: Client,
    pool: sqlx::PgPool,
    cache_hits: Arc<AtomicU64>,
    cache_misses: Arc<AtomicU64>,
    lock_acquired: Arc<AtomicU64>,
    lock_contention: Arc<AtomicU64>,
    errors: Arc<AtomicU64>,
    fallback_count: Arc<AtomicU64>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CachedResponse {
    pub status: u16,
    pub body: String,
    pub content_type: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IdempotencyKey {
    pub key: String,
    pub ttl_seconds: u64,
}

#[derive(Debug)]
pub enum IdempotencyStatus {
    New,
    Processing,
    Completed(CachedResponse),
}

/// Value stored in the lock key: JSON with instance_id and locked_at (unix timestamp).
#[derive(Debug, Serialize, Deserialize)]
struct LockValue {
    instance_id: String,
    locked_at: u64,
}

fn cache_key(tenant_id: &str, key: &str) -> String {
    format!("idempotency:{}:{}", tenant_id, key)
}

fn lock_key(tenant_id: &str, key: &str) -> String {
    format!("idempotency:lock:{}:{}", tenant_id, key)
}

fn lock_value() -> String {
    let instance_id =
        std::env::var("INSTANCE_ID").unwrap_or_else(|_| std::process::id().to_string());
    let locked_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    serde_json::to_string(&LockValue {
        instance_id,
        locked_at,
    })
    .unwrap_or_else(|_| "processing".to_string())
}

impl IdempotencyService {
    pub fn new(
        redis_url: &str,
        pool: sqlx::PgPool,
        cache_hits: Arc<AtomicU64>,
        cache_misses: Arc<AtomicU64>,
        lock_acquired: Arc<AtomicU64>,
        lock_contention: Arc<AtomicU64>,
        errors: Arc<AtomicU64>,
        fallback_count: Arc<AtomicU64>,
    ) -> Result<Self, redis::RedisError> {
        let client = Client::open(redis_url)?;
        Ok(Self {
            client,
            pool,
            cache_hits,
            cache_misses,
            lock_acquired,
            lock_contention,
            errors,
            fallback_count,
        })
    }

    pub async fn check_idempotency(
        &self,
        tenant_id: &str,
        key: &str,
    ) -> Result<IdempotencyStatus, Box<dyn std::error::Error + Send + Sync>> {
        let cache_key = format!("idempotency:{}", key);
        let lock_key = format!("idempotency:lock:{}", key);
        
        match self.client.get_multiplexed_async_connection().await {
            Ok(mut conn) => {
                // Check if response is cached
                let cached: Option<String> = redis::cmd("GET")
                    .arg(&cache_key)
                    .query_async(&mut conn)
                    .await?;
                    
                if let Some(data) = cached {
                    self.cache_hits.fetch_add(1, Ordering::Relaxed);
                    let response: CachedResponse = serde_json::from_str(&data)
                        .map_err(|e| redis::RedisError::from((redis::ErrorKind::TypeError, "deserialization error", e.to_string())))?;
                    return Ok(IdempotencyStatus::Completed(response));
                }
                
                self.cache_misses.fetch_add(1, Ordering::Relaxed);
                
                // Try to acquire lock
                let acquired: bool = redis::cmd("SET")
                    .arg(&lock_key)
                    .arg("processing")
                    .arg("NX")
                    .arg("EX")
                    .arg(300) // 5 minute lock
                    .query_async(&mut conn)
                    .await?;
                    
                if acquired {
                    self.lock_acquired.fetch_add(1, Ordering::Relaxed);
                    Ok(IdempotencyStatus::New)
                } else {
                    self.lock_contention.fetch_add(1, Ordering::Relaxed);
                    Ok(IdempotencyStatus::Processing)
                }
            }
            Err(redis_err) => {
                // Redis failed, fall back to database
                tracing::warn!("Redis unavailable for idempotency check, falling back to database: {}", redis_err);
                self.fallback_count.fetch_add(1, Ordering::Relaxed);
                
                self.check_idempotency_db(key).await
            }
        }
    }

    async fn check_idempotency_db(
        &self,
        key: &str,
    ) -> Result<IdempotencyStatus, Box<dyn std::error::Error + Send + Sync>> {
        use chrono::{Duration, Utc};
        
        // Check if key exists in database
        if let Some(db_key) = crate::db::queries::check_idempotency_key(&self.pool, key).await? {
            match db_key.status.as_str() {
                "completed" => {
                    if let Some(response_json) = db_key.response {
                        let status = response_json.get("status").and_then(|v| v.as_u64()).unwrap_or(200) as u16;
                        let body = response_json.get("body").and_then(|v| v.as_str()).unwrap_or("{}").to_string();
                        let content_type = response_json.get("content_type").and_then(|v| v.as_str()).map(|s| s.to_string());
                        let cached = CachedResponse { status, body, content_type };
                        Ok(IdempotencyStatus::Completed(cached))
                    } else {
                        // No response stored, treat as processing
                        Ok(IdempotencyStatus::Processing)
                    }
                }
                "processing" => Ok(IdempotencyStatus::Processing),
                _ => Ok(IdempotencyStatus::Processing),
            }
        } else {
            // Key doesn't exist, try to insert as processing
            let expires_at = Utc::now() + Duration::hours(24);
            crate::db::queries::insert_idempotency_key(
                &self.pool,
                key,
                "processing",
                None,
                expires_at,
            ).await?;
            Ok(IdempotencyStatus::New)
        }
    }

    pub async fn store_response(
        &self,
        tenant_id: &str,
        key: &str,
        status: u16,
        body: String,
        content_type: Option<String>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let cache_key = format!("idempotency:{}", key);
        let lock_key = format!("idempotency:lock:{}", key);
        
        let cached = CachedResponse { status, body: body.clone(), content_type: content_type.clone() };
        let data = serde_json::to_string(&cached)?;
        
        match self.client.get_multiplexed_async_connection().await {
            Ok(mut conn) => {
                // Store response with 24 hour TTL
                redis::cmd("SETEX")
                    .arg(&cache_key)
                    .arg(86400)
                    .arg(&data)
                    .query_async::<_, ()>(&mut conn)
                    .await?;
                
                // Release lock
                redis::cmd("DEL")
                    .arg(&lock_key)
                    .query_async::<_, ()>(&mut conn)
                    .await?;
                
                Ok(())
            }
            Err(redis_err) => {
                // Redis failed, store in database
                tracing::warn!("Redis unavailable for storing idempotency response, storing in database: {}", redis_err);
                
                let response_json = serde_json::json!({
                    "status": status,
                    "body": body,
                    "content_type": content_type
                });
                let expires_at = chrono::Utc::now() + chrono::Duration::hours(24);
                
                crate::db::queries::update_idempotency_key_response(
                    &self.pool,
                    key,
                    &response_json,
                ).await?;
                
                Ok(())
            }
        }
    }

    pub async fn release_lock(&self, key: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let lock_key = format!("idempotency:lock:{}", key);
        
        match self.client.get_multiplexed_async_connection().await {
            Ok(mut conn) => {
                redis::cmd("DEL")
                    .arg(&lock_key)
                    .query_async::<_, ()>(&mut conn)
                    .await?;
                Ok(())
            }
            Err(_) => {
                // If Redis is down, we can't release the lock, but that's okay
                // The database fallback doesn't use locks in the same way
                Ok(())
            }
        }
    }

    pub async fn check_and_set(
        &self,
        key: &str,
        value: &str,
        ttl: Duration,
    ) -> Result<bool, RedisError> {
        let key = key.to_string();
        let value = value.to_string();
        let client = self.client.clone();
        self.cb
            .call(|| async move {
                let mut conn = client.get_multiplexed_async_connection().await?;
                let acquired: bool = redis::cmd("SET")
                    .arg(&key)
                    .arg(&value)
                    .arg("NX")
                    .arg("EX")
                    .arg(ttl.as_secs())
                    .query_async(&mut conn)
                    .await?;
                Ok(acquired)
            })
            .await
    }

    /// Returns the circuit breaker state: `"open"` or `"closed"`.
    pub fn circuit_state(&self) -> String {
        self.cb.state()
    }

    /// Background task: scan for stale locks (older than 2 minutes with no cached response)
    /// and delete them so the next request can reprocess.
    pub async fn recover_stale_locks(&self) -> Result<(), RedisError> {
        let client = self.client.clone();
        self.cb
            .call(|| async move {
                let mut conn = client.get_multiplexed_async_connection().await?;

                let lock_keys: Vec<String> = redis::cmd("KEYS")
                    .arg("idempotency:lock:*")
                    .query_async(&mut conn)
                    .await?;

                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();

                for lk in lock_keys {
                    let raw: Option<String> =
                        redis::cmd("GET").arg(&lk).query_async(&mut conn).await?;

                    let Some(raw) = raw else { continue };

                    let locked_at = serde_json::from_str::<LockValue>(&raw)
                        .map(|v| v.locked_at)
                        .unwrap_or(0);

                    if locked_at == 0 || now.saturating_sub(locked_at) < 120 {
                        continue;
                    }

                    // Lock key: idempotency:lock:{tenant_id}:{key}
                    // Cache key: idempotency:{tenant_id}:{key}
                    let ck = lk.replacen("idempotency:lock:", "idempotency:", 1);
                    let cached: Option<String> =
                        redis::cmd("GET").arg(&ck).query_async(&mut conn).await?;

                    if cached.is_none() {
                        tracing::warn!(lock_key = %lk, "Recovering stale idempotency lock");
                        redis::cmd("DEL")
                            .arg(&lk)
                            .query_async::<_, ()>(&mut conn)
                            .await?;
                    }
                }

                Ok(())
            })
            .await
    }
}

/// Extract tenant ID from `X-Tenant-Id` header; falls back to `"default"`.
fn extract_tenant_id(request: &Request<Body>) -> String {
    request
        .headers()
        .get("x-tenant-id")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .unwrap_or("default")
        .to_string()
}

/// Middleware to handle idempotency for webhook requests
pub async fn idempotency_middleware(
    State(service): State<IdempotencyService>,
    request: Request<Body>,
    next: Next<Body>,
) -> Response {
    let idempotency_key = match request.headers().get("x-idempotency-key") {
        Some(key) => match key.to_str() {
            Ok(k) => k.to_string(),
            Err(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "error": "Invalid idempotency key format"
                    })),
                )
                    .into_response();
            }
        },
        None => {
            return next.run(request).await;
        }
    };

    let tenant_id = extract_tenant_id(&request);

    match service
        .check_idempotency(&tenant_id, &idempotency_key)
        .await
    {
        Ok(IdempotencyStatus::New) => {
            let response: Response = next.run(request).await;

            if response.status().is_success() {
                let status = response.status().as_u16();
                let content_type = response.headers().get("content-type")
                    .and_then(|h| h.to_str().ok())
                    .map(|s| s.to_string());
                
                // Read the response body
                let body_bytes = match axum::body::to_bytes(response.into_body(), usize::MAX).await {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        tracing::error!("Failed to read response body for caching: {}", e);
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(serde_json::json!({"error": "Failed to cache response"})),
                        )
                            .into_response();
                    }
                };
                
                let body_string = if body_bytes.len() > 64 * 1024 {
                    // Body too large, cache status only
                    tracing::warn!("Response body exceeds 64KB limit, caching status only");
                    serde_json::json!({"status": "success"}).to_string()
                } else {
                    match std::str::from_utf8(&body_bytes) {
                        Ok(s) => s.to_string(),
                        Err(_) => {
                            // Binary body, base64 encode
                            use base64::Engine;
                            base64::engine::general_purpose::STANDARD.encode(&body_bytes)
                        }
                    }
                };
                
                // Recreate the response for the client
                let client_response = Response::builder()
                    .status(status)
                    .header("content-type", content_type.as_deref().unwrap_or("application/json"))
                    .body(Body::from(body_bytes))
                    .unwrap();

                if let Err(e) = service.store_response(&validated_key, status, body).await {
                    tracing::error!("Failed to store idempotency response: {}", e);
                }
                
                client_response
            } else {
                if let Err(e) = service.release_lock(&validated_key).await {
                    tracing::error!("Failed to release idempotency lock: {}", e);
                }
                response
            }
        }
        Ok(IdempotencyStatus::Processing) => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({
                "error": "Request is currently being processed",
                "retry_after": 5
            })),
        )
            .into_response(),
        Ok(IdempotencyStatus::Completed(cached)) => {
            let status = StatusCode::from_u16(cached.status).unwrap_or(StatusCode::OK);
            
            // Reconstruct the response body
            let body_bytes = if cached.body.starts_with("ey") || cached.body.contains("{") {
                // Assume it's JSON or base64
                if let Ok(json_value) = serde_json::from_str::<serde_json::Value>(&cached.body) {
                    // It's JSON
                    serde_json::to_vec(&json_value).unwrap_or_else(|_| cached.body.as_bytes().to_vec())
                } else {
                    // Try base64 decode
                    use base64::Engine;
                    base64::engine::general_purpose::STANDARD.decode(&cached.body).unwrap_or_else(|_| cached.body.as_bytes().to_vec())
                }
            } else {
                cached.body.as_bytes().to_vec()
            };
            
            let mut response_builder = Response::builder()
                .status(status)
                .header("x-idempotent-replayed", "true");
                
            if let Some(content_type) = &cached.content_type {
                response_builder = response_builder.header("content-type", content_type);
            }
            
            response_builder
                .body(Body::from(body_bytes))
                .unwrap_or_else(|_| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({"error": "Failed to reconstruct cached response"})),
                    )
                        .into_response()
                })
        }
        Err(e) => {
            service.errors.fetch_add(1, Ordering::Relaxed);
            tracing::error!("Idempotency check failed: {}", e);
            next.run(request).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_idempotency_key_success() {
        assert_eq!(validate_idempotency_key("abc123").unwrap(), "abc123");
        assert_eq!(validate_idempotency_key("abc-def_123.45").unwrap(), "abc-def_123.45");
        assert_eq!(validate_idempotency_key("  abc123  ").unwrap(), "abc123");
    }

    #[test]
    fn test_validate_idempotency_key_empty_or_whitespace() {
        assert!(validate_idempotency_key("").is_err());
        assert!(validate_idempotency_key("   ").is_err());
    }

    #[test]
    fn test_validate_idempotency_key_invalid_characters() {
        assert!(validate_idempotency_key("abc def").is_err());
        assert!(validate_idempotency_key("abc@def").is_err());
        assert!(validate_idempotency_key("abc/def").is_err());
        assert!(validate_idempotency_key("abc\tdef").is_err());
    }

    #[test]
    fn test_validate_idempotency_key_control_characters() {
        assert!(validate_idempotency_key("abc\n123").is_err());
        assert!(validate_idempotency_key("abc\r123").is_err());
        assert!(validate_idempotency_key("abc\x00").is_err());
    }

    #[test]
    fn test_validate_idempotency_key_length_limits() {
        let max_key = "a".repeat(IDEMPOTENCY_KEY_MAX_LENGTH);
        assert!(validate_idempotency_key(&max_key).is_ok());

        let too_long_key = "a".repeat(IDEMPOTENCY_KEY_MAX_LENGTH + 1);
        assert!(validate_idempotency_key(&too_long_key).is_err());
    }
}


