use crate::ApiState;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::lock_manager::{lock_registry, ActiveLockInfo};

    #[tokio::test]
    async fn test_list_active_locks_includes_aria_label() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        lock_registry()
            .register(ActiveLockInfo {
                resource: "aria-test-resource".to_string(),
                token: "aria-test-token".to_string(),
                acquired_at: now,
                ttl_secs: 30,
                expected_duration_secs: 30,
                overdue: false,
                aria_label: ActiveLockInfo::aria_label("aria-test-resource", false),
            })
            .await;

        let locks = lock_registry().snapshot().await;
        let entry = locks
            .iter()
            .find(|l| l.token == "aria-test-token")
            .expect("registered lock present in snapshot");

        assert!(!entry.aria_label.is_empty());
        assert!(entry.aria_label.contains("aria-test-resource"));
        assert!(entry.aria_label.contains("active"));

        let json = serde_json::to_value(entry).unwrap();
        assert!(json.get("aria_label").is_some());
    }
}
