pub mod export;
pub mod settlements;
pub mod webhook;
pub mod dlq;
pub mod admin;
// pub mod graphql;  // Temporarily disabled - backup file exists
pub mod search;

use crate::AppState;
use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let postgres_checker = crate::health::PostgresChecker::new(state.db.clone());
    let redis_checker = crate::health::RedisChecker::new(state.redis_url.clone());
    let horizon_checker = crate::health::HorizonChecker::new(state.horizon_client.clone());

    let health_response = crate::health::check_health(
        postgres_checker,
        redis_checker,
        horizon_checker,
        state.start_time,
    )
    .await;

    let status_code = match health_response.status.as_str() {
        "healthy" => StatusCode::OK,
        "degraded" => StatusCode::OK,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    };

    (status_code, Json(health_response))
}

/// Readiness probe endpoint for Kubernetes
/// Returns 200 when ready to accept traffic, 503 when draining or not ready
pub async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    if state.readiness.is_ready() {
        let response = ReadinessResponse {
            status: "ready".to_string(),
            draining: state.readiness.is_draining(),
        };
        (StatusCode::OK, Json(response))
    } else {
        let response = ReadinessResponse {
            status: "not_ready".to_string(),
            draining: state.readiness.is_draining(),
        };
        (StatusCode::SERVICE_UNAVAILABLE, Json(response))
    }
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ReadinessResponse {
    pub status: String,
    pub draining: bool,
}
