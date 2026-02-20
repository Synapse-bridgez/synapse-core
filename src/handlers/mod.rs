pub mod export;

use crate::AppState;
use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};

pub mod settlements;
pub mod webhook;

#[derive(Debug, Serialize, Deserialize)]
pub struct HealthStatus {
    status: String,
    version: String,
    db_primary: String,
    db_replica: Option<String>,
}

pub async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let health_check = state.pool_manager.health_check().await;
    
    let db_primary_status = if health_check.primary { "connected" } else { "disconnected" };
    let db_replica_status = if state.pool_manager.replica().is_some() {
        Some(if health_check.replica { "connected" } else { "disconnected" }.to_string())
    } else {
        None
    };

    let overall_healthy = health_check.primary && health_check.replica;

    let health_response = HealthStatus {
        status: if overall_healthy { "healthy" } else { "unhealthy" }.to_string(),
        version: "0.1.0".to_string(),
        db_primary: db_primary_status.to_string(),
        db_replica: db_replica_status,
    };

    let status_code = if overall_healthy {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (status_code, Json(health_response))
}

pub async fn callback_transaction(State(_state): State<AppState>) -> impl IntoResponse {
    StatusCode::NOT_IMPLEMENTED
}
