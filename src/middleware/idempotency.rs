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

    match service.check_idempotency(&idempotency_key).await {
        Ok(IdempotencyStatus::New) => {
            let response: Response = next.run(request).await;

            if response.status().is_success() {
                let status = response.status().as_u16();
                let body = serde_json::json!({"status": "success"}).to_string();

                if let Err(e) = service.store_response(&idempotency_key, status, body).await {
                    tracing::error!("Failed to store idempotency response: {}", e);
                }
            } else {
                if let Err(e) = service.release_lock(&idempotency_key).await {
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
