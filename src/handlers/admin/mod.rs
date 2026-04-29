pub mod bulk_status;
pub mod locks;
pub mod quota;
pub mod webhook_replay;

use crate::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use uuid;

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateFlagRequest {
    pub enabled: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateRolloutPercentageRequest {
    pub rollout_percentage: i32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateWebhookRateLimitRequest {
    pub max_delivery_rate: i32,
}

#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    pub flag_name: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// Create admin routes for queue management
pub fn admin_routes() -> Router<sqlx::PgPool> {
    Router::new()
        .route("/flags", get(get_flags))
        .route("/flags/:name", post(update_flag))
        .route("/flags/:name/rollout", post(update_rollout_percentage))
        .route("/feature-flags/history", get(get_flag_history))
        .route("/backup/status", get(get_backup_status))
        .route("/backup/verification-history", get(get_backup_verification_history))
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

pub async fn get_backup_status(State(state): State<AppState>) -> impl IntoResponse {
    match state.backup_service.get_progress().await {
        Some(progress) => (StatusCode::OK, Json(progress)).into_response(),
        None => (
            StatusCode::OK,
            Json(serde_json::json!({
                "phase": "idle",
                "progress_percentage": 0,
                "elapsed_seconds": 0,
                "estimated_remaining_seconds": null,
                "total_size_bytes": 0
            })),
        )
            .into_response(),
    }
}

pub async fn get_backup_verification_history(
    State(pool): State<sqlx::PgPool>,
    Query(params): Query<HistoryQuery>,
) -> impl IntoResponse {
    let limit = params.limit.unwrap_or(50).min(1000);
    let offset = params.offset.unwrap_or(0);

    match sqlx::query_as::<_, crate::services::backup::BackupVerificationLog>(
        r#"
        SELECT id, backup_filename, verification_status, row_count, latest_timestamp, error_message, verified_at
        FROM backup_verification_logs
        ORDER BY verified_at DESC
        LIMIT $1 OFFSET $2
        "#,
    )
    .bind(limit)
    .bind(offset)
    .fetch_all(&pool)
    .await
    {
        Ok(history) => (StatusCode::OK, Json(history)).into_response(),
        Err(e) => {
            tracing::error!("Failed to get backup verification history: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Failed to retrieve verification history"
                })),
            )
                .into_response()
        }
    }
}

/// GET /admin/instances — list active processor instances via Redis heartbeat keys.
pub async fn list_active_instances(State(state): State<crate::ApiState>) -> impl IntoResponse {
    let election = match crate::services::LeaderElection::new(&state.app_state.redis_url) {
        Ok(e) => e,
        Err(e) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": format!("Redis unavailable: {e}")})),
            )
                .into_response();
        }
    };

    let (instances_res, leader_res) =
        tokio::join!(election.list_active_instances(), election.current_leader(),);

    match (instances_res, leader_res) {
        (Ok(instances), Ok(leader)) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "instances": instances,
                "leader": leader,
                "count": instances.len(),
            })),
        )
            .into_response(),
        (Err(e), _) | (_, Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

pub async fn get_flags(State(state): State<AppState>) -> impl IntoResponse {
    match state.feature_flags.get_all().await {
        Ok(flags) => (StatusCode::OK, Json(flags)).into_response(),
        Err(e) => {
            tracing::error!("Failed to get feature flags: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Failed to retrieve feature flags"
                })),
            )
                .into_response()
        }
    }
}

pub async fn update_flag(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(payload): Json<UpdateFlagRequest>,
) -> impl IntoResponse {
    match state.feature_flags.update(&name, payload.enabled).await {
        Ok(flag) => (StatusCode::OK, Json(flag)).into_response(),
        Err(e) => {
            tracing::error!("Failed to update feature flag '{}': {}", name, e);
            (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": format!("Feature flag '{}' not found", name)
                })),
            )
                .into_response()
        }
    }
}

pub async fn update_rollout_percentage(
    State(state): State<AppState>,
    Path(name): Path<String>,
    Json(payload): Json<UpdateRolloutPercentageRequest>,
) -> impl IntoResponse {
    if payload.rollout_percentage < 0 || payload.rollout_percentage > 100 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "rollout_percentage must be between 0 and 100"
            })),
        )
            .into_response();
    }

    match state
        .feature_flags
        .update_rollout_percentage(&name, payload.rollout_percentage)
        .await
    {
        Ok(flag) => (StatusCode::OK, Json(flag)).into_response(),
        Err(e) => {
            tracing::error!(
                "Failed to update rollout percentage for '{}': {}",
                name,
                e
            );
            (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": format!("Feature flag '{}' not found", name)
                })),
            )
                .into_response()
        }
    }
}

pub async fn update_webhook_rate_limit(
    State(pool): State<sqlx::PgPool>,
    Path(endpoint_id): Path<uuid::Uuid>,
    Json(payload): Json<UpdateWebhookRateLimitRequest>,
) -> impl IntoResponse {
    if payload.max_delivery_rate <= 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "max_delivery_rate must be greater than 0"
            })),
        )
            .into_response();
    }

    match sqlx::query(
        r#"
        UPDATE webhook_endpoints
        SET max_delivery_rate = $1, updated_at = NOW()
        WHERE id = $2
        "#,
    )
    .bind(payload.max_delivery_rate)
    .bind(endpoint_id)
    .execute(&pool)
    .await
    {
        Ok(result) => {
            if result.rows_affected() == 0 {
                (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({
                        "error": "Webhook endpoint not found"
                    })),
                )
                    .into_response()
            } else {
                (
                    StatusCode::OK,
                    Json(serde_json::json!({
                        "message": "Rate limit updated successfully",
                        "endpoint_id": endpoint_id,
                        "max_delivery_rate": payload.max_delivery_rate
                    })),
                )
                    .into_response()
            }
        }
        Err(e) => {
            tracing::error!(
                "Failed to update webhook rate limit for {}: {}",
                endpoint_id,
                e
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Failed to update rate limit"
                })),
            )
                .into_response()
        }
    }
}

pub async fn get_flag_history(
    State(state): State<AppState>,
    Query(params): Query<HistoryQuery>,
) -> impl IntoResponse {
    let limit = params.limit.unwrap_or(50).min(1000);
    let offset = params.offset.unwrap_or(0);

    match state
        .feature_flags
        .get_audit_history(params.flag_name.as_deref(), limit, offset)
        .await
    {
        Ok(history) => (StatusCode::OK, Json(history)).into_response(),
        Err(e) => {
            tracing::error!("Failed to get feature flag history: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Failed to retrieve audit history"
                })),
            )
                .into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// Webhook endpoint health score handlers
// ---------------------------------------------------------------------------

/// GET /admin/webhooks/health
pub async fn list_webhook_health(State(state): State<crate::ApiState>) -> impl IntoResponse {
    match crate::services::webhook_dispatcher::list_endpoint_health(&state.app_state.db).await {
        Ok(health) => (StatusCode::OK, Json(health)).into_response(),
        Err(e) => {
            tracing::error!("Failed to list webhook endpoint health: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
}

/// POST /admin/tenants/reload — immediately reload tenant configs from DB
pub async fn reload_tenant_configs(
    State(state): State<crate::ApiState>,
) -> impl IntoResponse {
    match state.app_state.load_tenant_configs().await {
        Ok(()) => {
            let count = state.app_state.tenant_configs.read().await.len();
            tracing::info!(count, "Tenant configs reloaded via admin endpoint");
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "message": "Tenant configs reloaded",
                    "tenant_count": count
                })),
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!("Failed to reload tenant configs: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
}

/// GET /admin/webhooks/health/:id
pub async fn get_webhook_health(
    State(state): State<crate::ApiState>,
    Path(id): Path<uuid::Uuid>,
) -> impl IntoResponse {
    match crate::services::webhook_dispatcher::get_endpoint_health(&state.app_state.db, id).await {
        Ok(health) => (StatusCode::OK, Json(health)).into_response(),
        Err(crate::error::AppError::NotFound(msg)) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": msg })),
        )
            .into_response(),
        Err(e) => {
            tracing::error!("Failed to get webhook endpoint health {}: {}", id, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
}
