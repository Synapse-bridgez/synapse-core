use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub mod webhook;
// pub mod graphql; // Temporarily disabled
pub mod settlements;
pub mod dlq;
pub mod admin;

// Keep the old HealthStatus for backward compatibility if needed
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct HealthStatus {
    status: String,
    version: String,
    db: String,
}

use crate::AppState;

/// Health check endpoint with dependency matrix
#[utoipa::path(
    get,
    path = "/health",
    responses(
        (status = 200, description = "Service is healthy", body = HealthResponse),
        (status = 503, description = "Service is unhealthy or degraded", body = HealthResponse)
    ),
    tag = "Health"
)]
pub async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let health_response = state.health_checker.check_all().await;
    
    let status_code = match health_response.status.as_str() {
        "healthy" => StatusCode::OK,
        "degraded" => StatusCode::OK, // Still return 200 for degraded
        _ => StatusCode::SERVICE_UNAVAILABLE,
    };

    (status_code, Json(health_response))
}

pub async fn callback_transaction(State(_state): State<AppState>) -> impl IntoResponse {
    StatusCode::NOT_IMPLEMENTED
}
