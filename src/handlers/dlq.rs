use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde_json::json;
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

async fn list_dlq(State(pool): State<PgPool>) -> Result<impl IntoResponse, AppError> {
    let entries = sqlx::query_as::<_, TransactionDlq>(
        "SELECT * FROM transaction_dlq ORDER BY moved_to_dlq_at DESC LIMIT 100",
    )
    .fetch_all(&pool)
    .await?;

    Ok(Json(json!({
        "dlq_entries": entries,
        "count": entries.len()
    })))
}

/// How a requeue attempt ended, as far as the caller is concerned.
#[derive(Debug, PartialEq, Eq)]
enum RequeueOutcome {
    /// The entry was moved back to `pending` and removed from the DLQ (200).
    Requeued,
    /// A transient failure (pool timeout, I/O) stopped the requeue. The entry
    /// is still in the DLQ and the same call can be retried later (202). The
    /// reason is a fixed, safe description, never the raw error text.
    Deferred(&'static str),
    /// A permanent failure (missing entry, invalid status transition, ...) (500).
    Failed,
}

/// Classifies the result of `TransactionProcessor::requeue_dlq`.
///
/// Transient means the same classes the processor retries on: database pool
/// timeouts and I/O errors. Everything else is permanent.
fn classify_requeue(result: &anyhow::Result<()>) -> RequeueOutcome {
    match result {
        Ok(()) => RequeueOutcome::Requeued,
        Err(e) => match e.downcast_ref::<sqlx::Error>() {
            Some(sqlx::Error::PoolTimedOut) => {
                RequeueOutcome::Deferred("database connection pool timed out")
            }
            Some(sqlx::Error::Io(_)) => RequeueOutcome::Deferred("database I/O error"),
            _ => RequeueOutcome::Failed,
        },
    }
}

async fn requeue_dlq(
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<axum::response::Response, AppError> {
    let processor = TransactionProcessor::new(pool);
    let result = processor.requeue_dlq(id).await;

    match classify_requeue(&result) {
        RequeueOutcome::Requeued => Ok(Json(json!({
            "message": "DLQ entry requeued successfully",
            "dlq_id": id
        }))
        .into_response()),
        RequeueOutcome::Deferred(reason) => {
            tracing::warn!(dlq_id = %id, reason, "DLQ requeue deferred after a transient failure");
            Ok((
                StatusCode::ACCEPTED,
                Json(json!({
                    "message": "DLQ entry was not requeued yet; retry later",
                    "dlq_id": id,
                    "reason": reason
                })),
            )
                .into_response())
        }
        // Permanent failure: 500 through AppError, which redacts the raw cause.
        RequeueOutcome::Failed => Err(AppError::from(
            result.expect_err("Failed outcome only comes from an Err"),
        )),
    }
}
