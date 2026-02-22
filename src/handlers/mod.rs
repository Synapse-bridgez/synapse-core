use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub mod webhook;
pub mod graphql;
pub mod settlements;
pub mod dlq;
pub mod admin;


#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct HealthStatus {
    status: String,
    version: String,
    db: String,
}

use crate::ApiState;

pub async fn health(State(state): State<ApiState>) -> impl IntoResponse {
    // Check database connectivity with SELECT 1 query
    let db_status = match sqlx::query("SELECT 1").execute(&state.app_state.db).await {
        Ok(_) => "connected",
        Err(_) => "disconnected",
    };

    let health_response = HealthStatus {
        status: if db_status == "connected" {
            "healthy".to_string()
        } else {
            "unhealthy".to_string()
        },
        version: "0.1.0".to_string(),
        db: db_status.to_string(),
    };

    // Return 503 if database is down, 200 otherwise
    let status_code = if db_status == "connected" {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (status_code, Json(health_response))
}

pub async fn callback_transaction(State(_state): State<AppState>) -> impl IntoResponse {
    StatusCode::NOT_IMPLEMENTED
}
