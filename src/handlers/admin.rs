use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
    Router,
};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::db::queries;
use crate::error::AppError;
use crate::services::TransactionProcessor;

pub fn admin_routes() -> Router<PgPool> {
    Router::new()
        .route("/status", get(get_queue_status))
        .route("/failed", get(get_failed_transactions))
        .route("/retry/:id", post(retry_transaction))
}

async fn get_queue_status(State(pool): State<PgPool>) -> Result<Json<Value>, AppError> {
    let status_counts = queries::get_queue_status(&pool).await?;
    Ok(Json(json!(status_counts)))
}

async fn get_failed_transactions(State(pool): State<PgPool>) -> Result<Json<Value>, AppError> {
    let failed_txs = queries::get_failed_transactions(&pool).await?;
    Ok(Json(json!({
        "failed_transactions": failed_txs,
        "count": failed_txs.len()
    })))
}

async fn retry_transaction(
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, AppError> {
    let processor = TransactionProcessor::new(pool.clone());
    
    // First, try as DLQ ID (existing behavior)
    if let Ok(_) = processor.requeue_dlq(id).await {
        return Ok(Json(json!({
            "message": "Transaction requeued successfully",
            "id": id
        })));
    }

    // If that fails, try to find a DLQ entry for this transaction ID
    let dlq_entry = sqlx::query("SELECT id FROM transaction_dlq WHERE transaction_id = $1")
        .bind(id)
        .fetch_optional(&pool)
        .await?;

    if let Some(row) = dlq_entry {
        let dlq_id: Uuid = row.get("id");
        processor.requeue_dlq(dlq_id).await?;
        Ok(Json(json!({
            "message": "Transaction requeued successfully",
            "id": id,
            "dlq_id": dlq_id
        })))
    } else {
        Err(AppError::NotFound(format!("No failed transaction found with ID {}", id)))
    }
}
