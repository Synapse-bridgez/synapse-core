pub mod webhook_replay;

use crate::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateFlagRequest {
    pub enabled: bool,
}

/// Create admin routes for queue management
pub fn admin_routes() -> Router<sqlx::PgPool> {
    Router::new().route("/flags", get(|| async { StatusCode::NOT_IMPLEMENTED }))
}

/// Create webhook replay admin routes
pub fn webhook_replay_routes() -> Router<sqlx::PgPool> {
    Router::new()
        .route("/webhooks/failed", get(webhook_replay::list_failed_webhooks))
        .route("/webhooks/replay/:id", post(webhook_replay::replay_webhook))
        .route("/webhooks/replay/batch", post(webhook_replay::batch_replay_webhooks))
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
