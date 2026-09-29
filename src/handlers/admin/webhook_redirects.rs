//! Webhook endpoint redirect management API (Issue #1259)
//!
//! Provides time-bounded redirect configuration for safe webhook traffic migration
//! during infrastructure changes. Enables simultaneous delivery to both primary
//! and redirect URLs with independent retry/breaker tracking.

use crate::db::queries::{self, CreateRedirectRequest, WebhookEndpointRedirect};
use crate::error::AppError;
use crate::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize)]
pub struct CreateRedirectResponse {
    pub id: i64,
    pub endpoint_id: String,
    pub redirect_url: String,
    pub enabled: bool,
    pub created_at: String,
    pub expires_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RedirectResponse {
    pub id: i64,
    pub endpoint_id: String,
    pub redirect_url: String,
    pub enabled: bool,
    pub created_at: String,
    pub started_at: Option<String>,
    pub expires_at: String,
    pub cancelled_at: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ActiveRedirectsResponse {
    pub endpoint_id: String,
    pub redirects: Vec<RedirectResponse>,
}

/// Create a webhook endpoint redirect
/// POST /admin/webhooks/endpoints/:id/redirects
pub async fn create_webhook_redirect(
    State(state): State<AppState>,
    Path(endpoint_id): Path<Uuid>,
    Json(req): Json<CreateRedirectRequest>,
) -> Result<impl IntoResponse, AppError> {
    // Validate request
    if req.redirect_url.trim().is_empty() {
        return Err(AppError::BadRequest(
            "redirect_url cannot be empty".to_string(),
        ));
    }

    if req.redirect_url.len() > 2048 {
        return Err(AppError::BadRequest(
            "redirect_url exceeds maximum length of 2048 characters".to_string(),
        ));
    }

    if req.expires_at <= Utc::now() {
        return Err(AppError::BadRequest(
            "expires_at must be in the future".to_string(),
        ));
    }

    // Check that expires_at is not more than 30 days in the future (safety boundary)
    let max_duration = chrono::Duration::days(30);
    if req.expires_at > Utc::now() + max_duration {
        return Err(AppError::BadRequest(
            "expires_at must be within 30 days".to_string(),
        ));
    }

    let redirect = queries::create_webhook_redirect(
        &state.db_pool,
        endpoint_id,
        &req.redirect_url,
        req.expires_at,
        req.metadata,
    )
    .await
    .map_err(|e| {
        tracing::error!("Failed to create webhook redirect: {}", e);
        AppError::InternalServerError("Failed to create redirect".to_string())
    })?;

    Ok((
        StatusCode::CREATED,
        Json(CreateRedirectResponse {
            id: redirect.id,
            endpoint_id: redirect.endpoint_id.to_string(),
            redirect_url: redirect.redirect_url,
            enabled: redirect.enabled,
            created_at: redirect.created_at.to_rfc3339(),
            expires_at: redirect.expires_at.to_rfc3339(),
        }),
    ))
}

/// List active redirects for an endpoint
/// GET /admin/webhooks/endpoints/:id/redirects
pub async fn list_active_redirects(
    State(state): State<AppState>,
    Path(endpoint_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let redirects = queries::get_active_endpoint_redirects(&state.db_pool, endpoint_id)
        .await
        .map_err(|e| {
            tracing::error!("Failed to list webhook redirects: {}", e);
            AppError::InternalServerError("Failed to list redirects".to_string())
        })?;

    let response_redirects = redirects
        .into_iter()
        .map(|r| RedirectResponse {
            id: r.id,
            endpoint_id: r.endpoint_id.to_string(),
            redirect_url: r.redirect_url,
            enabled: r.enabled,
            created_at: r.created_at.to_rfc3339(),
            started_at: r.started_at.map(|dt| dt.to_rfc3339()),
            expires_at: r.expires_at.to_rfc3339(),
            cancelled_at: r.cancelled_at.map(|dt| dt.to_rfc3339()),
        })
        .collect();

    Ok((
        StatusCode::OK,
        Json(ActiveRedirectsResponse {
            endpoint_id: endpoint_id.to_string(),
            redirects: response_redirects,
        }),
    ))
}

/// Cancel a webhook redirect
/// POST /admin/webhooks/redirects/:id/cancel
pub async fn cancel_webhook_redirect(
    State(state): State<AppState>,
    Path(redirect_id): Path<i64>,
) -> Result<impl IntoResponse, AppError> {
    let cancelled = queries::cancel_webhook_redirect(&state.db_pool, redirect_id)
        .await
        .map_err(|e| {
            tracing::error!("Failed to cancel webhook redirect: {}", e);
            AppError::InternalServerError("Failed to cancel redirect".to_string())
        })?;

    if !cancelled {
        return Err(AppError::NotFound("Redirect not found or already cancelled".to_string()));
    }

    Ok((
        StatusCode::OK,
        Json(serde_json::json!({
            "redirect_id": redirect_id,
            "message": "Redirect cancelled successfully"
        })),
    ))
}
