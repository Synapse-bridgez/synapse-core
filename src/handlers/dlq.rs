use axum::{
    extract::{Path, State},
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::db::models::TransactionDlq;
use crate::error::AppError;
use crate::services::TransactionProcessor;
use crate::swap::recovery::{self, Intervention};
use crate::ApiState;

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

async fn requeue_dlq(
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let processor = TransactionProcessor::new(pool);
    processor.requeue_dlq(id).await?;

    Ok(Json(json!({
        "message": "DLQ entry requeued successfully",
        "dlq_id": id
    })))
}

/// Operator-supplied audit context for an intervention.
#[derive(Debug, Deserialize)]
pub struct InterventionRequest {
    pub actor: String,
    pub reason: String,
}

async fn run_intervention(
    state: ApiState,
    id: Uuid,
    op: Intervention,
    req: InterventionRequest,
) -> Result<impl IntoResponse, AppError> {
    let actor = req.actor.trim();
    let reason = req.reason.trim();
    if actor.is_empty() || reason.is_empty() {
        return Err(AppError::BadRequest(
            "actor and reason are required".to_string(),
        ));
    }
    if actor.len() > 255 || reason.len() > 1000 {
        return Err(AppError::BadRequest(
            "actor must be at most 255 bytes and reason at most 1000 bytes".to_string(),
        ));
    }

    let outcome = recovery::intervene(&state.app_state.db, id, op, actor, reason).await?;
    Ok(Json(outcome))
}

/// POST /admin/transactions/:id/dlq/retry — requeue a stuck transaction to `pending`.
pub async fn retry_dlq_transaction(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
    Json(req): Json<InterventionRequest>,
) -> Result<impl IntoResponse, AppError> {
    run_intervention(state, id, Intervention::Retry, req).await
}

/// POST /admin/transactions/:id/dlq/force-complete — mark a stuck transaction `completed`.
pub async fn force_complete_dlq_transaction(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
    Json(req): Json<InterventionRequest>,
) -> Result<impl IntoResponse, AppError> {
    run_intervention(state, id, Intervention::ForceComplete, req).await
}

/// POST /admin/transactions/:id/dlq/force-cancel — mark a stuck transaction `failed`.
pub async fn force_cancel_dlq_transaction(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
    Json(req): Json<InterventionRequest>,
) -> Result<impl IntoResponse, AppError> {
    run_intervention(state, id, Intervention::ForceCancel, req).await
}
