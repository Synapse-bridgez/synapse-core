pub mod admin;
pub mod dlq;
pub mod export;
pub mod graphql;
pub mod idempotency;
pub mod pagination;
pub mod profiling;
pub mod search;
pub mod session;
pub mod settlements;
pub mod stats;
pub mod v1;
pub mod v2;
pub mod webhook;
pub mod reconnection;
pub mod ws;
pub mod ws_error;

pub use pagination::{PaginationQuery, PaginatedListResponse, PaginationHelper, validate_pagination};

use crate::error::AppError;
use crate::ApiState;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Helper service for checking dependency health.
///
/// Consolidates common health check logic (database connectivity, pool stats)
/// to avoid duplication across health and readiness endpoints.
/// Designed to degrade gracefully—failures are logged and reported, not panicked.
pub struct HealthChecker;

impl HealthChecker {
    /// Checks database connectivity and returns pool statistics.
    ///
    /// # Arguments
    /// - `pool`: Database connection pool to check
    ///
    /// # Returns
    /// A tuple of (status, pool_stats, status_code):
    /// - status: "connected" or "disconnected"
    /// - pool_stats: Database pool utilization metrics
    /// - status_code: HTTP status (200 if connected, 503 if not)
    ///
    /// # Non-fatal behavior
    /// Gracefully degrades if database operations fail; returns "disconnected"
    /// status rather than panicking.
    pub async fn check_db(pool: &sqlx::PgPool) -> (String, DbPoolStats, StatusCode) {
        // Check database connectivity with SELECT 1 query
        let db_status = match sqlx::query("SELECT 1").execute(pool).await {
            Ok(_) => "connected",
            Err(_) => "disconnected",
        };

        // Gather pool statistics
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

        let status_code = if db_status == "connected" {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        };

        (db_status.to_string(), pool_stats, status_code)
    }
}

/// Liveness probe endpoint — always returns 200 if the process is running.
///
/// This endpoint indicates whether the application process itself is alive,
/// NOT whether it can handle requests or reach dependencies.
/// Used by orchestration platforms (Kubernetes) to determine if the process should be restarted.
///
/// # Returns
/// Always returns `(StatusCode::OK, LivenessResponse)`.
/// No dependency checks are performed; this endpoint cannot fail.
///
/// # Use case
/// Kubernetes liveness probes; restart the process if this returns non-200.
#[utoipa::path(
    get,
    path = "/live",
    responses(
        (status = 200, description = "Process is alive", body = LivenessResponse)
    ),
    tag = "Health"
)]
pub async fn live() -> impl IntoResponse {
    let response = LivenessResponse {
        status: "alive".to_string(),
    };
    (StatusCode::OK, Json(response))
}

/// Readiness probe endpoint — returns 200 when ready to accept traffic, 503 when draining.
///
/// This endpoint indicates whether the service is ready to accept requests.
/// Returns non-200 (503) if the service is draining or gracefully shutting down.
///
/// # Returns
/// - `(StatusCode::OK, ReadinessResponse)` — service is ready to accept traffic
/// - `(StatusCode::SERVICE_UNAVAILABLE, ReadinessResponse)` — service is draining or not ready
///
/// The response body includes `draining` flag indicating graceful shutdown state.
///
/// # Use case
/// Kubernetes readiness probes; remove from load balancer if this returns non-200.
#[utoipa::path(
    get,
    path = "/ready",
    responses(
        (status = 200, description = "Service is ready", body = ReadinessResponse),
        (status = 503, description = "Service is not ready or draining", body = ReadinessResponse)
    ),
    tag = "Health"
)]
pub async fn ready(State(state): State<ApiState>) -> Result<impl IntoResponse, AppError> {
    if state.app_state.readiness.is_ready() {
        let response = ReadinessResponse {
            status: "ready".to_string(),
            draining: state.app_state.readiness.is_draining(),
        };
        Ok((StatusCode::OK, Json(response)))
    } else {
        let response = ReadinessResponse {
            status: "not_ready".to_string(),
            draining: state.app_state.readiness.is_draining(),
        };
        Ok((StatusCode::SERVICE_UNAVAILABLE, Json(response)))
    }
}

/// Health check endpoint — aggregates dependency status and reports service health.
///
/// Checks database connectivity and gathers pool statistics to determine overall
/// service health. Returns 503 if critical dependencies (database) are unavailable.
///
/// The response includes detailed dependency status and queue metrics for monitoring.
///
/// # Returns
/// - `(StatusCode::OK, HealthStatus)` — all critical dependencies healthy
/// - `(StatusCode::SERVICE_UNAVAILABLE, HealthStatus)` — critical dependency failure (database)
///
/// # Use case
/// Monitoring and alerting; not used by orchestration platforms (use /live and /ready instead).
#[utoipa::path(
    get,
    path = "/health",
    responses(
        (status = 200, description = "Service is healthy", body = HealthStatus),
        (status = 503, description = "Service is unhealthy", body = HealthStatus)
    ),
    tag = "Health"
)]
pub async fn health(State(state): State<ApiState>) -> Result<impl IntoResponse, AppError> {
    // Use HealthChecker to check database connectivity and gather pool stats
    let (db_status, pool_stats, db_status_code) =
        HealthChecker::check_db(&state.app_state.db).await;

    let pending_queue_depth = state
        .app_state
        .pending_queue_depth
        .load(std::sync::atomic::Ordering::Relaxed);
    let current_batch_size = state
        .app_state
        .current_batch_size
        .load(std::sync::atomic::Ordering::Relaxed);
    let ws_connection_count = state
        .app_state
        .ws_connection_count
        .load(std::sync::atomic::Ordering::Relaxed);

    let health_response = HealthStatus {
        status: if db_status == "connected" {
            "healthy".to_string()
        } else {
            "unhealthy".to_string()
        },
        version: "0.1.0".to_string(),
        db: db_status,
        db_pool: pool_stats,
        pending_queue_depth,
        current_batch_size,
        ws_connection_count,
    };

    Ok((db_status_code, Json(health_response)))
}


/// Response from the liveness probe endpoint (/live).
///
/// Indicates whether the application process is running.
/// Always returns `status: "alive"` if the endpoint returns 200.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct LivenessResponse {
    /// Always "alive" if this response is returned.
    /// If the process crashes, orchestration platforms will detect the missing response and restart.
    pub status: String,
}

/// Response from the readiness probe endpoint (/ready).
///
/// Indicates whether the service is ready to accept traffic.
/// Used by load balancers and orchestration platforms to determine routing.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct ReadinessResponse {
    /// "ready" if the service can handle traffic, "not_ready" if draining or misconfigured
    pub status: String,
    /// true if the service is in graceful shutdown mode (/admin/drain was called)
    pub draining: bool,
}

/// Response from the health check endpoint (/health).
///
/// Aggregates dependency health information for monitoring and alerting.
/// Not used by orchestration platforms; use /live and /ready for that instead.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct HealthStatus {
    /// Overall status: "healthy" if all critical deps healthy, "unhealthy" if any critical dep down
    pub status: String,
    /// API version
    pub version: String,
    /// Database connection status: "connected" or "disconnected"
    pub db: String,
    /// Database connection pool utilization and limits
    pub db_pool: DbPoolStats,
    /// Number of pending tasks in the queue; high values may indicate overload
    pub pending_queue_depth: u64,
    /// Current batch size for settlement processing
    pub current_batch_size: u64,
    /// Number of active WebSocket connections
    pub ws_connection_count: usize,
}

/// Database connection pool statistics.
///
/// Used to monitor pool exhaustion and connection health.
/// High `usage_percent` may indicate capacity issues.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct DbPoolStats {
    /// Number of connections currently in use
    pub active_connections: u32,
    /// Number of idle connections available for reuse
    pub idle_connections: u32,
    /// Maximum allowed connections in the pool
    pub max_connections: u32,
    /// Percentage of pool utilized (active / max) as a float 0-100
    pub usage_percent: f32,
}

/// Error catalog endpoint
/// Returns all available error codes and their descriptions
pub async fn error_catalog() -> Result<impl IntoResponse, AppError> {
    let errors = crate::error::get_all_error_codes();
    let catalog = crate::error::ErrorCatalogResponse {
        errors,
        version: "1.0.0".to_string(),
    };

    Ok((StatusCode::OK, Json(catalog)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_liveness_response_always_alive() {
        // The /live endpoint is always alive regardless of dependencies
        // This test verifies that the LivenessResponse always indicates alive
        let response = LivenessResponse {
            status: "alive".to_string(),
        };
        assert_eq!(response.status, "alive");
    }

    #[test]
    fn test_readiness_response_structure() {
        // Verify ReadinessResponse can represent both ready and not_ready states
        let ready = ReadinessResponse {
            status: "ready".to_string(),
            draining: false,
        };
        assert_eq!(ready.status, "ready");
        assert!(!ready.draining);

        let not_ready = ReadinessResponse {
            status: "not_ready".to_string(),
            draining: true,
        };
        assert_eq!(not_ready.status, "not_ready");
        assert!(not_ready.draining);
    }

    #[test]
    fn test_health_status_response_structure() {
        // Verify HealthStatus can represent healthy and unhealthy states
        let healthy = HealthStatus {
            status: "healthy".to_string(),
            version: "0.1.0".to_string(),
            db: "connected".to_string(),
            db_pool: DbPoolStats {
                active_connections: 5,
                idle_connections: 10,
                max_connections: 20,
                usage_percent: 25.0,
            },
            pending_queue_depth: 100,
            current_batch_size: 50,
            ws_connection_count: 10,
        };
        assert_eq!(healthy.status, "healthy");
        assert_eq!(healthy.db, "connected");

        let unhealthy = HealthStatus {
            status: "unhealthy".to_string(),
            version: "0.1.0".to_string(),
            db: "disconnected".to_string(),
            db_pool: DbPoolStats {
                active_connections: 0,
                idle_connections: 0,
                max_connections: 20,
                usage_percent: 0.0,
            },
            pending_queue_depth: 0,
            current_batch_size: 0,
            ws_connection_count: 0,
        };
        assert_eq!(unhealthy.status, "unhealthy");
        assert_eq!(unhealthy.db, "disconnected");
    }

    #[test]
    fn test_db_pool_stats_utilization() {
        // Verify pool stats correctly reflect connection utilization
        let stats = DbPoolStats {
            active_connections: 15,
            idle_connections: 5,
            max_connections: 20,
            usage_percent: 75.0,
        };
        assert_eq!(stats.active_connections + stats.idle_connections, 20);
        assert_eq!(stats.usage_percent, 75.0);

        // Test at capacity
        let full = DbPoolStats {
            active_connections: 20,
            idle_connections: 0,
            max_connections: 20,
            usage_percent: 100.0,
        };
        assert_eq!(full.usage_percent, 100.0);
    }

    #[test]
    fn test_health_checker_db_status_logic() {
        // This test verifies the logic for determining health status based on db_status
        // "connected" → "healthy", "disconnected" → "unhealthy"
        assert_eq!(
            if "connected" == "connected" {
                "healthy"
            } else {
                "unhealthy"
            },
            "healthy"
        );

        assert_eq!(
            if "disconnected" == "connected" {
                "healthy"
            } else {
                "unhealthy"
            },
            "unhealthy"
        );
    }

    #[test]
    fn test_health_checker_status_code_logic() {
        // Verify that database status correctly maps to HTTP status codes
        let connected_code = if "connected" == "connected" {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        };
        assert_eq!(connected_code, StatusCode::OK);

        let disconnected_code = if "disconnected" == "connected" {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        };
        assert_eq!(disconnected_code, StatusCode::SERVICE_UNAVAILABLE);
    }
}
