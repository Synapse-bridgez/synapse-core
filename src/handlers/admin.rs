use crate::AppState;
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateFlagRequest {
    pub enabled: bool,
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
