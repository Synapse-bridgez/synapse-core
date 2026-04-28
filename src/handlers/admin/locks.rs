use crate::ApiState;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};

/// GET /admin/locks — list all active distributed locks held by this instance.
pub async fn list_active_locks(State(_state): State<ApiState>) -> impl IntoResponse {
    let locks = crate::services::lock_manager::lock_registry().snapshot().await;

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
