use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Clone)]
pub struct IdempotencyService {
    client: Client,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IdempotencyKey {
    pub key: String,
    pub ttl_seconds: u64,
}

impl IdempotencyService {
    pub fn new(redis_url: &str) -> Result<Self, redis::RedisError> {
        let client = Client::open(redis_url)?;
        Ok(Self { client })
    }

    pub async fn check_and_set(&self, key: &str, value: &str, ttl: Duration) -> Result<bool, redis::RedisError> {
        let mut conn = self.client.get_connection()?;
        let result: Option<String> = conn.set_nx_ex(key, value, ttl.as_secs())?;
        Ok(result.is_some())
    }
}

#[derive(Debug)]
pub enum IdempotencyStatus {
    New,
    Processing,
    Completed(CachedResponse),
}

/// Middleware to handle idempotency for webhook requests
pub async fn idempotency_middleware(
    State(service): State<IdempotencyService>,
    request: Request<Body>,
    next: Next<Body>,
) -> Response {
    // Extract idempotency key from request
    // This could be from headers, query params, or body
    // For now, we'll extract from a custom header
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
            // If no idempotency key provided, proceed without idempotency check
            return next.run(request).await;
        }
    };

    // Check idempotency status
    match service.check_idempotency(&idempotency_key).await {
        Ok(IdempotencyStatus::New) => {
            // Process the request
            let response: Response = next.run(request).await;

            // If successful (2xx), cache the response
            if response.status().is_success() {
                // Extract response body and status
                let status = response.status().as_u16();
                
                // For simplicity, we'll store a success marker
                // In production, you might want to capture the actual response body
                let body = serde_json::json!({"status": "success"}).to_string();
                
                if let Err(e) = service.store_response(&idempotency_key, status, body).await {
                    tracing::error!("Failed to store idempotency response: {}", e);
                }
            } else {
                // Release lock on failure
                if let Err(e) = service.release_lock(&idempotency_key).await {
                    tracing::error!("Failed to release idempotency lock: {}", e);
                }
            }

            response
        }
        Ok(IdempotencyStatus::Processing) => {
            // Request is currently being processed
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
            // Return cached response
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
            // On Redis failure, proceed with request (fail open)
            next.run(request).await
        }
    }
}
