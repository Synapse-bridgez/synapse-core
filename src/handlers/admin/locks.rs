use crate::ApiState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};

/// GET /admin/locks — list all active distributed locks held by this instance.
pub async fn list_active_locks(State(_state): State<ApiState>) -> impl IntoResponse {
    let locks = crate::services::lock_manager::lock_registry()
        .snapshot()
        .await;

    let overdue_count = locks.iter().filter(|l| l.overdue).count();

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "active_locks": locks,
            "total": locks.len(),
            "overdue": overdue_count,
        })),
    )
        .into_response()
}

/// POST /admin/locks/:resource/force-release — force-release a distributed
/// lock regardless of its current owner. Idempotent: releasing a lock that
/// has already been released or expired between listing and this call
/// returns success (`released: false`) rather than an error.
pub async fn force_release_lock(
    State(state): State<ApiState>,
    Path(resource): Path<String>,
) -> impl IntoResponse {
    match crate::services::lock_manager::force_release_lock(
        &state.app_state.redis_url,
        &resource,
    )
    .await
    {
        Ok(released) => {
            tracing::warn!(
                resource = %resource,
                released,
                "Admin force-released distributed lock"
            );
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "resource": resource,
                    "released": released,
                    "message": if released {
                        "Lock released"
                    } else {
                        "Lock was already released or expired"
                    },
                })),
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!(resource = %resource, error = %e, "Failed to force-release lock");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "failed to release lock"})),
            )
                .into_response()
        }
    }
}
