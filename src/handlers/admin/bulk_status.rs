use crate::db::queries::{bulk_update_transaction_status, BulkUpdateError, BulkUpdateResult};
use crate::error::AppError;
use crate::{ApiState, AppState};
use axum::{extract::State, response::IntoResponse, Json};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct BulkStatusRequest {
    pub transaction_ids: Vec<Uuid>,
    pub status: String,
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BulkStatusResponse {
    pub updated: usize,
    pub failed: usize,
    pub errors: Vec<BulkUpdateError>,
}

pub async fn bulk_update_status(
    State(state): State<AppState>,
    Json(payload): Json<BulkStatusRequest>,
) -> Result<impl IntoResponse, AppError> {
    run_bulk_update(&state.db, payload).await
}

/// ApiState-compatible wrapper used by the main router.
pub async fn bulk_update_status_api(
    State(api_state): State<ApiState>,
    Json(payload): Json<BulkStatusRequest>,
) -> Result<impl IntoResponse, AppError> {
    run_bulk_update(&api_state.app_state.db, payload).await
}

async fn run_bulk_update(
    pool: &sqlx::PgPool,
    payload: BulkStatusRequest,
) -> Result<impl IntoResponse, AppError> {
    if payload.transaction_ids.is_empty() {
        return Err(AppError::BadRequest(
            "transaction_ids must not be empty".to_string(),
        ));
    }
    if payload.transaction_ids.len() > 500 {
        return Err(AppError::BadRequest(
            "transaction_ids must not exceed 500 items per request".to_string(),
        ));
    }

    let valid_statuses = ["pending", "processing", "completed", "failed"];
    if !valid_statuses.contains(&payload.status.as_str()) {
        return Err(AppError::Validation(format!(
            "invalid status '{}', must be one of: {}",
            payload.status,
            valid_statuses.join(", ")
        )));
    }

    let result: BulkUpdateResult = bulk_update_transaction_status(
        pool,
        &payload.transaction_ids,
        &payload.status,
        payload.reason.as_deref(),
        "admin",
    )
    .await
    .map_err(|e| AppError::DatabaseError(e.to_string()))?;

    Ok(Json(BulkStatusResponse {
        updated: result.updated,
        failed: result.failed,
        errors: result.errors,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bulk_status_request_deserializes() {
        let json = r#"{
            "transaction_ids": ["00000000-0000-0000-0000-000000000001"],
            "status": "failed",
            "reason": "manual override"
        }"#;
        let req: BulkStatusRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.status, "failed");
        assert_eq!(req.reason.as_deref(), Some("manual override"));
        assert_eq!(req.transaction_ids.len(), 1);
    }
}
