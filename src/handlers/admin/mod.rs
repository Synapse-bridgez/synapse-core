pub mod bulk_status;
pub mod locks;
pub mod quota;
pub mod reconciliation;
pub mod webhook_replay;

use crate::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use uuid;

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateFlagRequest {
    pub enabled: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateWebhookRateLimitRequest {
    pub max_delivery_rate: i32,
}

/// Create admin routes for queue management
pub fn admin_routes() -> Router<sqlx::PgPool> {
    Router::new().route("/flags", get(|| async { StatusCode::NOT_IMPLEMENTED }))
}

/// Create webhook replay admin routes
pub fn webhook_replay_routes() -> Router<sqlx::PgPool> {
    Router::new()
        .route(
            "/webhooks/failed",
            get(webhook_replay::list_failed_webhooks),
        )
        .route("/webhooks/replay/:id", post(webhook_replay::replay_webhook))
        .route(
            "/webhooks/replay/batch",
            post(webhook_replay::batch_replay_webhooks),
        )
        .route(
            "/webhooks/endpoints/:id/rate-limit",
            post(update_webhook_rate_limit),
        )
}

/// GET /admin/instances — list active processor instances via Redis heartbeat keys.
pub async fn list_active_instances(State(state): State<crate::ApiState>) -> Result<impl IntoResponse, AppError> {
    let election = crate::services::LeaderElection::new(&state.app_state.redis_url)?;

    let (instances, leader) =
        tokio::try_join!(election.list_active_instances(), election.current_leader())?;

    Ok((
        StatusCode::OK,
        Json(serde_json::json!({
            "instances": instances,
            "leader": leader,
            "count": instances.len(),
        })),
    ))
}

pub async fn get_flags(State(state): State<AppState>) -> Result<impl IntoResponse, AppError> {
    let flags = state.feature_flags.get_all().await?;
    Ok((StatusCode::OK, Json(flags)))
}

pub async fn update_flag(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(payload): Json<UpdateFlagRequest>,
) -> Result<impl IntoResponse, AppError> {
    let flag = state.feature_flags.update(&name, payload.enabled).await
        .map_err(|_| AppError::NotFound(format!("Feature flag '{}' not found", name)))?;
    
    Ok((StatusCode::OK, Json(flag)))
}

pub async fn update_webhook_rate_limit(
    State(pool): State<sqlx::PgPool>,
    Path(endpoint_id): Path<uuid::Uuid>,
    Json(payload): Json<UpdateWebhookRateLimitRequest>,
) -> Result<impl IntoResponse, AppError> {
    if payload.max_delivery_rate <= 0 {
        return Err(AppError::BadRequest("max_delivery_rate must be greater than 0".to_string()));
    }

    let result = sqlx::query(
        r#"
        UPDATE webhook_endpoints
        SET max_delivery_rate = $1, updated_at = NOW()
        WHERE id = $2
        "#,
    )
    .bind(payload.max_delivery_rate)
    .bind(endpoint_id)
    .execute(&pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Webhook endpoint not found".to_string()));
    }

    Ok((
        StatusCode::OK,
        Json(serde_json::json!({
            "message": "Rate limit updated successfully",
            "endpoint_id": endpoint_id,
            "max_delivery_rate": payload.max_delivery_rate
        })),
    ))
}

// ---------------------------------------------------------------------------
// Webhook endpoint health score handlers
// ---------------------------------------------------------------------------

/// GET /admin/webhooks/health
pub async fn list_webhook_health(State(state): State<crate::ApiState>) -> Result<impl IntoResponse, AppError> {
    let health = crate::services::webhook_dispatcher::list_endpoint_health(&state.app_state.db).await?;
    Ok((StatusCode::OK, Json(health)))
}

/// POST /admin/tenants/reload — immediately reload tenant configs from DB
pub async fn reload_tenant_configs(
    State(state): State<crate::ApiState>,
) -> Result<impl IntoResponse, AppError> {
    state.app_state.load_tenant_configs().await?;
    let count = state.app_state.tenant_configs.read().await.len();
    tracing::info!(count, "Tenant configs reloaded via admin endpoint");
    Ok((
        StatusCode::OK,
        Json(serde_json::json!({
            "message": "Tenant configs reloaded",
            "tenant_count": count
        })),
    ))
}

/// GET /admin/webhooks/health/:id
pub async fn get_webhook_health(
    State(state): State<crate::ApiState>,
    Path(id): Path<uuid::Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let health = crate::services::webhook_dispatcher::get_endpoint_health(&state.app_state.db, id).await?;
    Ok((StatusCode::OK, Json(health)))
}
