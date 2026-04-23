use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use redis::Client;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

const IDEMPOTENCY_KEY_MIN_LENGTH: usize = 1;
const IDEMPOTENCY_KEY_MAX_LENGTH: usize = 255;

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
    ) -> Result<bool, redis::RedisError> {
        let mut conn = self.client.get_multiplexed_async_connection().await?;
        
        let acquired: bool = redis::cmd("SET")
            .arg(key)
            .arg(value)
            .arg("NX")
            .arg("EX")
            .arg(ttl.as_secs())
            .query_async(&mut conn)
            .await?;
            
        Ok(acquired)
    }
}

/// Validates an idempotency key according to requirements:
/// - Length: min 1, max 255 characters
/// - Only alphanumeric, hyphens, underscores, and dots allowed
/// - No control characters or whitespace
/// - Trims leading/trailing whitespace before validation
pub fn validate_idempotency_key(key: &str) -> Result<String, String> {
    // Trim whitespace from the key
    let trimmed_key = key.trim();

    // Check empty after trimming
    if trimmed_key.len() < IDEMPOTENCY_KEY_MIN_LENGTH {
        return Err("Idempotency key cannot be empty or whitespace only".to_string());
    }

    // Check length
    if trimmed_key.len() > IDEMPOTENCY_KEY_MAX_LENGTH {
        return Err(format!(
            "Idempotency key exceeds maximum length of {} characters",
            IDEMPOTENCY_KEY_MAX_LENGTH
        ));
    }

    // Check for invalid characters (only alphanumeric, hyphens, underscores, dots)
    if !trimmed_key
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err(
            "Idempotency key must contain only alphanumeric characters, hyphens, underscores, and dots"
                .to_string(),
        );
    }

    // Check for control characters (just to be safe)
    if trimmed_key.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("Idempotency key cannot contain control characters or whitespace".to_string());
    }

    Ok(trimmed_key.to_string())
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

    // Validate the idempotency key
    let validated_key = match validate_idempotency_key(&idempotency_key) {
        Ok(key) => key,
        Err(error_message) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": error_message
                })),
            )
                .into_response();
        }
    };

    match service.check_idempotency(&validated_key).await {
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
        Ok(IdempotencyStatus::Processing) => {
            (
                StatusCode::TOO_MANY_REQUESTS,
                Json(serde_json::json!({
                    "error": "Request is currently being processed",
                    "retry_after": 5
                })),
            )
                .into_response()
        }
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


