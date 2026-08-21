use axum::{
    extract::{Path, State},
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde_json::json;
use uuid::Uuid;

use crate::db::models::TransactionDlq;
use crate::error::AppError;
use crate::services::TransactionProcessor;
use crate::ApiState;

pub fn dlq_routes() -> Router<ApiState> {
    Router::new()
        .route("/dlq", get(list_dlq))
        .route("/dlq/:id/requeue", post(requeue_dlq))
}

async fn list_dlq(State(state): State<ApiState>) -> Result<impl IntoResponse, AppError> {
    let entries = sqlx::query_as::<_, TransactionDlq>(
        "SELECT * FROM transaction_dlq ORDER BY moved_to_dlq_at DESC LIMIT 100",
    )
    .fetch_all(&state.app_state.db)
    .await?;

    Ok(Json(json!({
        "dlq_entries": entries,
        "count": entries.len()
    })))
}

async fn requeue_dlq(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let processor = TransactionProcessor::new(state.app_state.db);
    processor.requeue_dlq(id).await?;

    Ok(Json(json!({
        "message": "DLQ entry requeued successfully",
        "dlq_id": id
    })))
}
