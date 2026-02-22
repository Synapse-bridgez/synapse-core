use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub mod webhook;
pub mod settlements;
pub mod graphql;
pub mod settlements;
pub mod dlq;
pub mod admin;


#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct HealthStatus {
    status: String,
    version: String,
    db: String,
    db_pool: DbPoolStats,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DbPoolStats {
    active_connections: u32,
    idle_connections: u32,
    max_connections: u32,
    usage_percent: f32,
}

use crate::ApiState;

pub async fn health(State(state): State<ApiState>) -> impl IntoResponse {
    // Check database connectivity with SELECT 1 query
    let db_status = match sqlx::query("SELECT 1").execute(&state.app_state.db).await {
        Ok(_) => "connected",
        Err(_) => "disconnected",
    };

    // Gather pool statistics
    let pool = &state.app_state.db;
    let active_connections = pool.size();
    let idle_connections = pool.num_idle();
    let max_connections = pool.options().get_max_connections();
    let usage_percent = (active_connections as f32 / max_connections as f32) * 100.0;

    let pool_stats = DbPoolStats {
        active_connections,
        idle_connections,
        max_connections,
        usage_percent,
    };

    let health_response = HealthStatus {
        status: if db_status == "connected" {
            "healthy".to_string()
        } else {
            "unhealthy".to_string()
        },
        version: "0.1.0".to_string(),
        db: db_status.to_string(),
        db_pool: pool_stats,
    };

    // Return 503 if database is down, 200 otherwise
    let status_code = if db_status == "connected" {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (status_code, Json(health_response))
}

pub async fn callback_transaction(State(_state): State<ApiState>) -> impl IntoResponse {
    StatusCode::NOT_IMPLEMENTED
}
