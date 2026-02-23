pub mod export;
pub mod settlements;
pub mod webhook;
pub mod dlq;
pub mod admin;
pub mod v1;
pub mod v2;

use crate::AppState;
use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[utoipa::path(
    get,
    path = "/health",
    responses(
        (status = 200, description = "Service is healthy", body = HealthStatus),
        (status = 503, description = "Service is unhealthy", body = HealthStatus)
    ),
    tag = "Health"
)]
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
        idle_connections: idle_connections as u32,
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

/// Error catalog endpoint
/// Returns all available error codes and their descriptions
pub async fn error_catalog() -> impl IntoResponse {
    let errors = crate::error::get_all_error_codes();
    let catalog = crate::error::ErrorCatalogResponse {
        errors,
        version: "1.0.0".to_string(),
    };
    
    (StatusCode::OK, Json(catalog))
}
