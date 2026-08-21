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

type RedisCBInner = StateMachine<failure_policy::ConsecutiveFailures<backoff::EqualJittered>, ()>;

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
    #[serde(default)]
    pub encoding: BodyEncoding,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BodyEncoding {
    #[default]
    Utf8,
    Base64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IdempotencyKey {
    pub key: String,
    pub ttl_seconds: u64,
}

#[derive(Debug)]
pub enum IdempotencyStatus {
    New { lock_token: Option<String> },
    Processing,
    Completed(CachedResponse),
}

/// Value stored in the lock key: JSON with instance_id and locked_at (unix timestamp).
#[derive(Debug, Serialize, Deserialize)]
struct LockValue {
    instance_id: String,
    locked_at: u64,
    #[serde(default)]
    token: String,
}

fn _cache_key(tenant_id: &str, key: &str) -> String {
    format!("idempotency:{tenant_id}:{key}")
}

fn _lock_key(tenant_id: &str, key: &str) -> String {
    format!("idempotency:lock:{tenant_id}:{key}")
}

fn _lock_value(token: &str) -> String {
    let instance_id =
        std::env::var("INSTANCE_ID").unwrap_or_else(|_| std::process::id().to_string());
    let locked_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    serde_json::to_string(&LockValue {
        instance_id,
        locked_at,
        token: token.to_string(),
    })
    .expect("serializing an idempotency lock value cannot fail")
}

impl IdempotencyService {
    #[allow(clippy::too_many_arguments)]
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
        let cache_key = _cache_key(tenant_id, key);
        let lock_key = _lock_key(tenant_id, key);

        match self.client.get_multiplexed_async_connection().await {
            Ok(mut conn) => {
                // Check if response is cached
                let cached: Option<String> = redis::cmd("GET")
                    .arg(&cache_key)
                    .query_async(&mut conn)
                    .await?;

                if let Some(data) = cached {
                    self.cache_hits.fetch_add(1, Ordering::Relaxed);
                    let response: CachedResponse = serde_json::from_str(&data).map_err(|e| {
                        redis::RedisError::from((
                            redis::ErrorKind::TypeError,
                            "deserialization error",
                            e.to_string(),
                        ))
                    })?;
                    return Ok(IdempotencyStatus::Completed(response));
                }

                self.cache_misses.fetch_add(1, Ordering::Relaxed);

                // Try to acquire lock; store a JSON lock value so
                // recover_stale_locks can inspect the locked_at timestamp.
                let lock_token = uuid::Uuid::new_v4().to_string();
                let acquired: bool = redis::cmd("SET")
                    .arg(&lock_key)
                    .arg(_lock_value(&lock_token))
                    .arg("NX")
                    .arg("EX")
                    .arg(300) // 5 minute lock
                    .query_async(&mut conn)
                    .await?;

                if acquired {
                    self.lock_acquired.fetch_add(1, Ordering::Relaxed);
                    Ok(IdempotencyStatus::New {
                        lock_token: Some(lock_token),
                    })
                } else {
                    self.lock_contention.fetch_add(1, Ordering::Relaxed);
                    Ok(IdempotencyStatus::Processing)
                }
            }
            Err(redis_err) => {
                // Redis failed, fall back to database
                tracing::warn!(
                    "Redis unavailable for idempotency check, falling back to database: {}",
                    redis_err
                );
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
                        let status = response_json
                            .get("status")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(200) as u16;
                        let body = response_json
                            .get("body")
                            .and_then(|v| v.as_str())
                            .unwrap_or("{}")
                            .to_string();
                        let content_type = response_json
                            .get("content_type")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        let encoding = response_json
                            .get("encoding")
                            .and_then(|v| serde_json::from_value(v.clone()).ok())
                            .unwrap_or_default();
                        let cached = CachedResponse {
                            status,
                            body,
                            content_type,
                            encoding,
                        };
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
            )
            .await?;
            Ok(IdempotencyStatus::New { lock_token: None })
        }
    }

    pub async fn store_response(
        &self,
        tenant_id: &str,
        key: &str,
        response: CachedResponse,
        lock_token: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if lock_token.is_none() {
            return self.store_response_db(key, &response).await;
        }

        let cache_key = _cache_key(tenant_id, key);
        let lock_key = _lock_key(tenant_id, key);
        let data = serde_json::to_string(&response)?;

        match self.client.get_multiplexed_async_connection().await {
            Ok(mut conn) => {
                // Store and release as one ownership-checked transaction. A
                // worker whose lock expired or was recovered cannot overwrite
                // the new owner's response or release its lock.
                redis::Script::new(STORE_RESPONSE_AND_RELEASE_SCRIPT)
                    .key(&lock_key)
                    .key(&cache_key)
                    .arg(lock_token.expect("checked above"))
                    .arg(86400)
                    .arg(&data)
                    .invoke_async::<_, u32>(&mut conn)
                    .await?;

                Ok(())
            }
            Err(redis_err) => {
                // Redis failed, store in database
                tracing::warn!(
                    "Redis unavailable for storing idempotency response, storing in database: {}",
                    redis_err
                );

                self.store_response_db(key, &response).await
            }
        }
    }

    async fn store_response_db(
        &self,
        key: &str,
        response: &CachedResponse,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let response_json = serde_json::to_value(response)?;
        crate::db::queries::update_idempotency_key_response(&self.pool, key, &response_json)
            .await?;
        Ok(())
    }

    pub async fn release_lock(
        &self,
        tenant_id: &str,
        key: &str,
        lock_token: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let lock_key = _lock_key(tenant_id, key);

        match self.client.get_multiplexed_async_connection().await {
            Ok(mut conn) => {
                if let Some(lock_token) = lock_token {
                    redis::Script::new(COMPARE_AND_DELETE_TOKEN_SCRIPT)
                        .key(&lock_key)
                        .arg(lock_token)
                        .invoke_async::<_, u32>(&mut conn)
                        .await?;
                }
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
        let mut conn = self
            .client
            .get_multiplexed_async_connection()
            .await
            .map_err(RedisError::Redis)?;
        let acquired: bool = redis::cmd("SET")
            .arg(key)
            .arg(value)
            .arg("NX")
            .arg("EX")
            .arg(ttl.as_secs())
            .query_async(&mut conn)
            .await
            .map_err(RedisError::Redis)?;
        Ok(acquired)
    }

    /// Returns the circuit breaker state: always `"closed"` (no CB in this service).
    pub fn circuit_state(&self) -> String {
        "closed".to_string()
    }

    /// Background task: scan for stale locks (older than 2 minutes with no cached response)
    /// and delete them so the next request can reprocess.
    pub async fn recover_stale_locks(&self) -> Result<(), RedisError> {
        let mut conn = self
            .client
            .get_multiplexed_async_connection()
            .await
            .map_err(RedisError::Redis)?;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut cursor = 0_u64;
        loop {
            let (next_cursor, lock_keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg("idempotency:lock:*")
                .arg("COUNT")
                .arg(100)
                .query_async(&mut conn)
                .await
                .map_err(RedisError::Redis)?;

            for lk in lock_keys {
                let raw: Option<String> = redis::cmd("GET")
                    .arg(&lk)
                    .query_async(&mut conn)
                    .await
                    .map_err(RedisError::Redis)?;

                let Some(raw) = raw else { continue };

                let locked_at = serde_json::from_str::<LockValue>(&raw)
                    .map(|v| v.locked_at)
                    .unwrap_or(0);

                if locked_at == 0 || now.saturating_sub(locked_at) < 120 {
                    continue;
                }

                let ck = lk.replacen("idempotency:lock:", "idempotency:", 1);
                let cached: Option<String> = redis::cmd("GET")
                    .arg(&ck)
                    .query_async(&mut conn)
                    .await
                    .map_err(RedisError::Redis)?;

                if cached.is_none() {
                    tracing::warn!(lock_key = %lk, "Recovering stale idempotency lock");
                    redis::Script::new(COMPARE_AND_DELETE_VALUE_SCRIPT)
                        .key(&lk)
                        .arg(&raw)
                        .invoke_async::<_, u32>(&mut conn)
                        .await
                        .map_err(RedisError::Redis)?;
                }
            }

            cursor = next_cursor;
            if cursor == 0 {
                break;
            }
        }

        Ok(())
    }
}

const COMPARE_AND_DELETE_TOKEN_SCRIPT: &str = r#"
local raw = redis.call('GET', KEYS[1])
if not raw then return 0 end
local ok, lock = pcall(cjson.decode, raw)
if ok and lock.token == ARGV[1] then
    return redis.call('DEL', KEYS[1])
end
return 0
"#;

const COMPARE_AND_DELETE_VALUE_SCRIPT: &str = r#"
if redis.call('GET', KEYS[1]) == ARGV[1] then
    return redis.call('DEL', KEYS[1])
end
return 0
"#;

const STORE_RESPONSE_AND_RELEASE_SCRIPT: &str = r#"
local raw = redis.call('GET', KEYS[1])
if not raw then return 0 end
local ok, lock = pcall(cjson.decode, raw)
if not ok or lock.token ~= ARGV[1] then return 0 end
redis.call('SETEX', KEYS[2], ARGV[2], ARGV[3])
redis.call('DEL', KEYS[1])
return 1
"#;

fn encode_body(body: &[u8]) -> (String, BodyEncoding) {
    match std::str::from_utf8(body) {
        Ok(body) => (body.to_string(), BodyEncoding::Utf8),
        Err(_) => {
            use base64::Engine;
            (
                base64::engine::general_purpose::STANDARD.encode(body),
                BodyEncoding::Base64,
            )
        }
    }
}

fn decode_body(cached: &CachedResponse) -> Result<Vec<u8>, base64::DecodeError> {
    match cached.encoding {
        BodyEncoding::Utf8 => Ok(cached.body.as_bytes().to_vec()),
        BodyEncoding::Base64 => {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD.decode(&cached.body)
        }
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

fn idempotency_trace_span(idempotency_key: &str, tenant_id: &str) -> tracing::Span {
    tracing::info_span!(
        "idempotency.check",
        idempotency_key = %idempotency_key,
        tenant_id = %tenant_id
    )
}

/// Middleware to handle idempotency for webhook requests
pub async fn idempotency_middleware(
    State(service): State<IdempotencyService>,
    request: Request<Body>,
    next: Next<Body>,
) -> Response {
    let idempotency_key = match request.headers().get("x-idempotency-key") {
        Some(key) => match key.to_str() {
            Ok(k) => match validate_idempotency_key(k) {
                Ok(validated) => validated,
                Err(e) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(serde_json::json!({ "error": e.to_string() })),
                    )
                        .into_response();
                }
            },
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
    let span = idempotency_trace_span(&idempotency_key, &tenant_id);
    let _enter = span.enter();

    match service
        .check_idempotency(&tenant_id, &idempotency_key)
        .await
    {
        Ok(IdempotencyStatus::New { lock_token }) => {
            let response: Response = next.run(request).await;

            if response.status().is_success() {
                let status = response.status().as_u16();
                let content_type = response
                    .headers()
                    .get("content-type")
                    .and_then(|h| h.to_str().ok())
                    .map(|s| s.to_string());

                // Read the response body
                let body_bytes = match hyper::body::to_bytes(response.into_body()).await {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        tracing::error!("Failed to read response body for caching: {}", e);
                        if let Err(release_error) = service
                            .release_lock(&tenant_id, &idempotency_key, lock_token.as_deref())
                            .await
                        {
                            tracing::error!(
                                "Failed to release idempotency lock: {}",
                                release_error
                            );
                        }
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(serde_json::json!({"error": "Failed to cache response"})),
                        )
                            .into_response();
                    }
                };

                let cacheable = body_bytes.len() <= 64 * 1024;

                if cacheable {
                    let (body_string, encoding) = encode_body(&body_bytes);
                    if let Err(e) = service
                        .store_response(
                            &tenant_id,
                            &idempotency_key,
                            CachedResponse {
                                status,
                                body: body_string,
                                content_type: content_type.clone(),
                                encoding,
                            },
                            lock_token.as_deref(),
                        )
                        .await
                    {
                        tracing::error!("Failed to store idempotency response: {}", e);
                    }
                } else {
                    tracing::warn!("Response body exceeds 64KB limit; not caching response");
                    if let Err(e) = service
                        .release_lock(&tenant_id, &idempotency_key, lock_token.as_deref())
                        .await
                    {
                        tracing::error!("Failed to release idempotency lock: {}", e);
                    }
                }

                // Recreate the response for the client without changing its
                // body bytes or content type.
                let mut client_response_builder = Response::builder().status(status);
                if let Some(content_type) = &content_type {
                    client_response_builder =
                        client_response_builder.header("content-type", content_type);
                }
                client_response_builder
                    .body(axum::body::boxed(Body::from(body_bytes)))
                    .unwrap()
            } else {
                if let Err(e) = service
                    .release_lock(&tenant_id, &idempotency_key, lock_token.as_deref())
                    .await
                {
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

            let body_bytes = match decode_body(&cached) {
                Ok(body) => body,
                Err(error) => {
                    tracing::error!(%error, "Invalid cached idempotency response encoding");
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
            };

            let mut response_builder = Response::builder()
                .status(status)
                .header("x-idempotent-replayed", "true");

            if let Some(content_type) = &cached.content_type {
                response_builder = response_builder.header("content-type", content_type);
            }

            response_builder
                .body(axum::body::boxed(Body::from(body_bytes)))
                .unwrap_or_else(|_| {
                    Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .body(axum::body::boxed(Body::from(
                            r#"{"error":"Failed to reconstruct cached response"}"#,
                        )))
                        .unwrap()
                })
        }
        Err(e) => {
            service.errors.fetch_add(1, Ordering::Relaxed);
            tracing::error!("Idempotency check failed: {}", e);
            next.run(request).await
        }
    }
}

pub const IDEMPOTENCY_KEY_MAX_LENGTH: usize = 255;

/// Validate and normalise an idempotency key.
/// - Trims surrounding whitespace
/// - Rejects empty / whitespace-only keys
/// - Rejects keys exceeding [`IDEMPOTENCY_KEY_MAX_LENGTH`]
/// - Rejects keys containing characters outside `[A-Za-z0-9\-_.]`
pub fn validate_idempotency_key(key: &str) -> Result<String, crate::error::AppError> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Err(crate::error::AppError::BadRequest(
            "Idempotency key must not be empty".into(),
        ));
    }
    if trimmed.len() > IDEMPOTENCY_KEY_MAX_LENGTH {
        return Err(crate::error::AppError::BadRequest(format!(
            "Idempotency key exceeds maximum length of {}",
            IDEMPOTENCY_KEY_MAX_LENGTH
        )));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(crate::error::AppError::BadRequest(
            "Idempotency key contains invalid characters".into(),
        ));
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::HashMap,
        fmt,
        sync::{Arc, Mutex},
    };
    use tracing::field::{Field, Visit};
    use tracing_subscriber::{
        layer::{Context, SubscriberExt},
        registry::Registry,
        Layer,
    };

    struct FieldCollector {
        fields: Mutex<HashMap<String, String>>,
    }

    impl FieldCollector {
        fn new() -> Self {
            Self {
                fields: Mutex::new(HashMap::new()),
            }
        }
    }

    impl Visit for FieldCollector {
        fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
            let mut fields = self.fields.lock().unwrap();
            fields.insert(field.name().to_string(), format!("{:?}", value));
        }

        fn record_i64(&mut self, field: &Field, value: i64) {
            let mut fields = self.fields.lock().unwrap();
            fields.insert(field.name().to_string(), value.to_string());
        }

        fn record_u64(&mut self, field: &Field, value: u64) {
            let mut fields = self.fields.lock().unwrap();
            fields.insert(field.name().to_string(), value.to_string());
        }

        fn record_bool(&mut self, field: &Field, value: bool) {
            let mut fields = self.fields.lock().unwrap();
            fields.insert(field.name().to_string(), value.to_string());
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            let mut fields = self.fields.lock().unwrap();
            fields.insert(field.name().to_string(), value.to_string());
        }

        fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
            let mut fields = self.fields.lock().unwrap();
            fields.insert(field.name().to_string(), value.to_string());
        }
    }

    struct CaptureSpanLayer {
        captured: Arc<Mutex<Vec<HashMap<String, String>>>>,
    }

    impl<S> Layer<S> for CaptureSpanLayer
    where
        S: tracing::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
    {
        fn on_new_span(
            &self,
            attrs: &tracing::span::Attributes<'_>,
            _id: &tracing::Id,
            _ctx: Context<'_, S>,
        ) {
            let mut visitor = FieldCollector::new();
            attrs.record(&mut visitor);
            let fields = visitor.fields.lock().unwrap().clone();
            self.captured.lock().unwrap().push(fields);
        }
    }

    #[test]
    fn test_idempotency_trace_span_records_key_and_tenant() {
        let captured = Arc::new(Mutex::new(Vec::new()));
        let layer = CaptureSpanLayer {
            captured: captured.clone(),
        };

        let subscriber = Registry::default().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            let span = idempotency_trace_span("test-key-123", "tenant-a");
            let _enter = span.enter();
            tracing::info!("idempotency event");
        });

        let spans = captured.lock().unwrap();
        assert_eq!(spans.len(), 1, "Expected exactly one captured span");

        let fields = &spans[0];
        let idempotency_key = fields
            .get("idempotency_key")
            .map(|s| s.trim_matches('"').to_string());
        let tenant_id = fields
            .get("tenant_id")
            .map(|s| s.trim_matches('"').to_string());

        assert_eq!(idempotency_key.as_deref(), Some("test-key-123"));
        assert_eq!(tenant_id.as_deref(), Some("tenant-a"));
    }

    #[test]
    fn test_validate_idempotency_key_success() {
        assert_eq!(validate_idempotency_key("abc123").unwrap(), "abc123");
        assert_eq!(
            validate_idempotency_key("abc-def_123.45").unwrap(),
            "abc-def_123.45"
        );
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
