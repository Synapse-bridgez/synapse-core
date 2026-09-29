//! Webhook retry policy management API (Issue #1258)
//!
//! Enables per-endpoint configurable retry policy with platform-wide safety boundaries.

use crate::db::queries::{self, RetryPolicyRequest};
use crate::error::AppError;
use crate::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize)]
pub struct RetryPolicyResponse {
    pub endpoint_id: String,
    pub retry_policy: RetryPolicy,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_attempts: i32,
    pub base_delay_secs: i32,
    pub multiplier: f64,
    pub max_delay_secs: i32,
}

/// Update retry policy for a webhook endpoint
/// POST /admin/webhooks/endpoints/:id/retry-policy
pub async fn update_webhook_retry_policy(
    State(state): State<AppState>,
    Path(endpoint_id): Path<Uuid>,
    Json(req): Json<RetryPolicyRequest>,
) -> Result<impl IntoResponse, AppError> {
    // Validate request against platform-wide safety boundaries
    req.validate()
        .map_err(|e| AppError::BadRequest(e))?;

    // Update the policy in the database
    queries::update_webhook_retry_policy(&state.db_pool, endpoint_id, req)
        .await
        .map_err(|e| {
            tracing::error!("Failed to update webhook retry policy: {}", e);
            match e {
                sqlx::Error::RowNotFound => {
                    AppError::NotFound("Webhook endpoint not found".to_string())
                }
                _ => AppError::InternalServerError("Failed to update retry policy".to_string()),
            }
        })?;

    Ok((
        StatusCode::OK,
        Json(serde_json::json!({
            "endpoint_id": endpoint_id.to_string(),
            "message": "Retry policy updated successfully"
        })),
    ))
}

/// Get retry policy for a webhook endpoint
/// GET /admin/webhooks/endpoints/:id/retry-policy
pub async fn get_webhook_retry_policy(
    State(state): State<AppState>,
    Path(endpoint_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let policy: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT retry_policy FROM webhook_endpoints WHERE id = $1"
    )
    .bind(endpoint_id)
    .fetch_optional(&state.db_pool)
    .await
    .map_err(|e| {
        tracing::error!("Failed to fetch webhook retry policy: {}", e);
        AppError::InternalServerError("Failed to fetch retry policy".to_string())
    })?
    .ok_or_else(|| AppError::NotFound("Webhook endpoint not found".to_string()))?;

    // Parse the policy with defaults
    let retry_policy: RetryPolicy = policy
        .as_ref()
        .and_then(|p| serde_json::from_value(p.clone()).ok())
        .unwrap_or(RetryPolicy {
            max_attempts: 5,
            base_delay_secs: 10,
            multiplier: 2.0,
            max_delay_secs: 300,
        });

    Ok((
        StatusCode::OK,
        Json(RetryPolicyResponse {
            endpoint_id: endpoint_id.to_string(),
            retry_policy,
        }),
    ))
}
