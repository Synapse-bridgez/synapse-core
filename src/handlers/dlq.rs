use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::db::models::TransactionDlq;
use crate::error::AppError;
use crate::services::TransactionProcessor;

pub fn dlq_routes() -> Router<PgPool> {
    Router::new()
        .route("/dlq", get(list_dlq))
        .route("/dlq/:id/requeue", post(requeue_dlq))
}

async fn list_dlq(State(pool): State<PgPool>) -> Result<Json<Value>, AppError> {
    let entries = sqlx::query_as::<_, TransactionDlq>(
        "SELECT * FROM transaction_dlq ORDER BY moved_to_dlq_at DESC LIMIT 100"
    )
    .fetch_all(&pool)
    .await?;

    Ok(Json(json!({
        "dlq_entries": entries,
        "count": entries.len()
    })))
}

async fn requeue_dlq(
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    let processor = TransactionProcessor::new(pool);
    processor
        .requeue_dlq(id)
        .await
        .map_err(|e| AppError::DatabaseError(e.to_string()))?;

    Ok(Json(json!({
        "message": "DLQ entry requeued successfully",
        "dlq_id": id
    })))
}
