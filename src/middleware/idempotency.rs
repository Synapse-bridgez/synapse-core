use axum::{
    body::Body,
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::time::Duration;

const IDEMPOTENCY_TTL: u64 = 86400; // 24 hours in seconds
const IDEMPOTENCY_PREFIX: &str = "idempotency:";

#[derive(Clone)]
pub struct IdempotencyService {
    redis_client: redis::Client,
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedResponse {
    status: u16,
    body: String,
}

impl IdempotencyService {
    pub fn new(redis_url: &str) -> anyhow::Result<Self> {
        let redis_client = redis::Client::open(redis_url)?;
        Ok(Self { redis_client })
    }

    /// Check if a request with this ID is already being processed or was completed
    pub async fn check_idempotency(&self, idempotency_key: &str) -> anyhow::Result<IdempotencyStatus> {
        let mut conn = self.redis_client.get_multiplexed_async_connection().await?;
        let key = format!("{}{}", IDEMPOTENCY_PREFIX, idempotency_key);

        // Try to get existing value
        let existing: Option<String> = conn.get(&key).await?;

        match existing {
            Some(value) => {
                if value == "PROCESSING" {
                    Ok(IdempotencyStatus::Processing)
                } else {
                    // Deserialize cached response
                    let cached: CachedResponse = serde_json::from_str(&value)?;
                    Ok(IdempotencyStatus::Completed(cached))
                }
            }
            None => {
                // Set PROCESSING lock with shorter TTL (5 minutes)
                let _: () = conn.set_ex(&key, "PROCESSING", 300).await?;
                Ok(IdempotencyStatus::New)
            }
        }
    }

    /// Store the successful response for future duplicate requests
    pub async fn store_response(
        &self,
        idempotency_key: &str,
        status: u16,
        body: String,
    ) -> anyhow::Result<()> {
        let mut conn = self.redis_client.get_multiplexed_async_connection().await?;
        let key = format!("{}{}", IDEMPOTENCY_PREFIX, idempotency_key);

        let cached = CachedResponse { status, body };
        let serialized = serde_json::to_string(&cached)?;

        // Store with 24-hour TTL
        let _: () = conn.set_ex(&key, serialized, IDEMPOTENCY_TTL).await?;
        Ok(())
    }

    /// Release the processing lock if an error occurs
    pub async fn release_lock(&self, idempotency_key: &str) -> anyhow::Result<()> {
        let mut conn = self.redis_client.get_multiplexed_async_connection().await?;
        let key = format!("{}{}", IDEMPOTENCY_PREFIX, idempotency_key);
        let _: () = conn.del(&key).await?;
        Ok(())
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
    request: Request,
    next: Next,
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
            let response = next.run(request).await;

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
