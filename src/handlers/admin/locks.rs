use crate::ApiState;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Serialize;

/// Serialized representation of a single active lock, including its owner.
#[derive(Serialize)]
struct LockResponse {
    owner: String,
    #[serde(flatten)]
    lock: crate::services::lock_manager::LockSnapshot,
}

/// GET /admin/locks — list all active distributed locks held by this instance.
pub async fn list_active_locks(State(_state): State<ApiState>) -> impl IntoResponse {
    let locks = crate::services::lock_manager::lock_registry()
        .snapshot()
        .await;

    let overdue_count = locks.iter().filter(|l| l.overdue).count();

    let locks: Vec<LockResponse> = locks
        .into_iter()
        .map(|lock| LockResponse {
            owner: lock.owner.clone(),
            lock,
        })
        .collect();

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_response_includes_owner() {
        let lock = crate::services::lock_manager::LockSnapshot {
            owner: "instance-a".to_string(),
            ..Default::default()
        };
        let response = LockResponse {
            owner: lock.owner.clone(),
            lock,
        };

        let json = serde_json::to_value(&response).expect("serialize lock response");
        assert_eq!(json.get("owner").and_then(|v| v.as_str()), Some("instance-a"));
    }
}
