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
use std::time::Duration;

const IDEMPOTENCY_KEY_MIN_LENGTH: usize = 1;
const IDEMPOTENCY_KEY_MAX_LENGTH: usize = 255;

#[derive(Clone)]
pub struct IdempotencyService {
    client: Client,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CachedResponse {
    pub status: u16,
    pub body: String,
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
    pub fn new(redis_url: &str) -> Result<Self, redis::RedisError> {
        let client = Client::open(redis_url)?;
        Ok(Self { client })
    }

    pub async fn check_idempotency(
        &self,
        key: &str,
    ) -> Result<IdempotencyStatus, redis::RedisError> {
        let cache_key = format!("idempotency:{}", key);
        let lock_key = format!("idempotency:lock:{}", key);
        
        let mut conn = self.client.get_multiplexed_async_connection().await?;
        
        // Check if response is cached
        let cached: Option<String> = redis::cmd("GET")
            .arg(&cache_key)
            .query_async(&mut conn)
            .await?;
            
        if let Some(data) = cached {
            let response: CachedResponse = serde_json::from_str(&data)
                .map_err(|e| redis::RedisError::from((redis::ErrorKind::TypeError, "deserialization error", e.to_string())))?;
            return Ok(IdempotencyStatus::Completed(response));
        }
        
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
            Ok(IdempotencyStatus::New)
        } else {
            Ok(IdempotencyStatus::Processing)
        }
    }

    pub async fn store_response(
        &self,
        key: &str,
        status: u16,
        body: String,
    ) -> Result<(), redis::RedisError> {
        let cache_key = format!("idempotency:{}", key);
        let lock_key = format!("idempotency:lock:{}", key);
        
        let cached = CachedResponse { status, body };
        let data = serde_json::to_string(&cached)
            .map_err(|e| redis::RedisError::from((redis::ErrorKind::TypeError, "serialization error", e.to_string())))?;
        
        let mut conn = self.client.get_multiplexed_async_connection().await?;
        
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

    pub async fn release_lock(&self, key: &str) -> Result<(), redis::RedisError> {
        let lock_key = format!("idempotency:lock:{}", key);
        let mut conn = self.client.get_multiplexed_async_connection().await?;
        
        redis::cmd("DEL")
            .arg(&lock_key)
            .query_async::<_, ()>(&mut conn)
            .await?;
            
        Ok(())
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
                let body = serde_json::json!({"status": "success"}).to_string();

                if let Err(e) = service.store_response(&validated_key, status, body).await {
                    tracing::error!("Failed to store idempotency response: {}", e);
                }
            } else {
                if let Err(e) = service.release_lock(&validated_key).await {
                    tracing::error!("Failed to release idempotency lock: {}", e);
                }
            }

            response
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
            (
                status,
                Json(serde_json::json!({
                    "cached": true,
                    "message": "Request already processed"
                })),
            )
                .into_response()
        }
        Err(e) => {
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


