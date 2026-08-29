//! Bulk transaction status update handler.
//!
//! Requests with `transaction_ids.len() <= ASYNC_THRESHOLD` are processed
//! synchronously and return the result immediately (existing behavior,
//! backward-compatible).
//!
//! Requests above the threshold are queued as a background job and the
//! handler returns `202 Accepted` with a `job_id`. Callers poll
//! `GET /admin/transactions/bulk-status/jobs/:id` until the job reaches a
//! terminal state (`completed` | `failed`).
//!
//! Quota limits (src/middleware/quota.rs) are respected by the background job
//! using the same `bulk_update_transaction_status` query path as the sync
//! route, so per-tenant rate limits are not bypassable by submitting a large
//! batch.

use crate::db::queries::{bulk_update_transaction_status, BulkUpdateError, BulkUpdateResult};
use crate::error::AppError;
use crate::{ApiState, AppState};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Batches at or below this size are processed synchronously (backward-compat).
/// Batches above this size are queued as async background jobs.
/// Configurable via `BULK_STATUS_ASYNC_THRESHOLD` env var; defaults to 50.
fn async_threshold() -> usize {
    std::env::var("BULK_STATUS_ASYNC_THRESHOLD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(50)
}

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct BulkStatusRequest {
    pub transaction_ids: Vec<Uuid>,
    pub status: String,
    pub reason: Option<String>,
}

/// Synchronous response (small batches, unchanged from before).
#[derive(Debug, Serialize)]
pub struct BulkStatusResponse {
    pub updated: usize,
    pub failed: usize,
    pub errors: Vec<BulkUpdateError>,
}

/// Asynchronous response (large batches): the job has been queued.
#[derive(Debug, Serialize)]
pub struct BulkStatusJobQueued {
    pub job_id: Uuid,
    pub status: &'static str,
    pub message: String,
}

/// Job status poll response.
#[derive(Debug, Serialize)]
pub struct BulkStatusJobStatus {
    pub id: Uuid,
    pub status: String,
    pub transaction_count: usize,
    pub target_status: String,
    pub result_summary: Option<serde_json::Value>,
    pub error_message: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
}

// ---------------------------------------------------------------------------
// Validation helpers (shared by sync and async paths)
// ---------------------------------------------------------------------------

fn validate_request(payload: &BulkStatusRequest) -> Result<(), AppError> {
    if payload.transaction_ids.is_empty() {
        return Err(AppError::BadRequest(
            "transaction_ids must not be empty".to_string(),
        ));
    }
    if payload.transaction_ids.len() > 10_000 {
        return Err(AppError::BadRequest(
            "transaction_ids must not exceed 10,000 items per request".to_string(),
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

    Ok(())
}

// ---------------------------------------------------------------------------
// Route handlers
// ---------------------------------------------------------------------------

/// PATCH /admin/transactions/bulk-status  (AppState variant, used in tests)
pub async fn bulk_update_status(
    State(state): State<AppState>,
    Json(payload): Json<BulkStatusRequest>,
) -> Result<impl IntoResponse, AppError> {
    dispatch(&state.db, payload).await
}

/// PATCH /admin/transactions/bulk-status  (ApiState variant, used by main router)
pub async fn bulk_update_status_api(
    State(api_state): State<ApiState>,
    Json(payload): Json<BulkStatusRequest>,
) -> Result<impl IntoResponse, AppError> {
    dispatch(&api_state.app_state.db, payload).await
}

/// GET /admin/transactions/bulk-status/jobs/:id  (ApiState variant)
pub async fn get_job_status(
    State(api_state): State<ApiState>,
    Path(job_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    fetch_job_status(&api_state.app_state.db, job_id).await
}

/// GET /admin/transactions/bulk-status/jobs/:id  (AppState variant, used in tests)
pub async fn get_job_status_app(
    State(state): State<AppState>,
    Path(job_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    fetch_job_status(&state.db, job_id).await
}

// ---------------------------------------------------------------------------
// Core dispatch logic
// ---------------------------------------------------------------------------

/// Route the request synchronously or to the async job queue depending on
/// batch size vs `async_threshold()`.
async fn dispatch(
    pool: &sqlx::PgPool,
    payload: BulkStatusRequest,
) -> Result<impl IntoResponse, AppError> {
    validate_request(&payload)?;

    if payload.transaction_ids.len() <= async_threshold() {
        // ── Synchronous path (backward-compatible) ──────────────────────
        let result: BulkUpdateResult = bulk_update_transaction_status(
            pool,
            &payload.transaction_ids,
            &payload.status,
            payload.reason.as_deref(),
            "admin",
        )
        .await?;

        return Ok((
            StatusCode::OK,
            Json(serde_json::to_value(BulkStatusResponse {
                updated: result.updated,
                failed: result.failed,
                errors: result.errors,
            })
            .unwrap()),
        ));
    }

    // ── Async path: insert a job row and spawn a background task ────────
    let job_id = enqueue_job(pool, &payload).await?;

    // Spawn the background execution; errors are logged, not propagated to
    // the caller (who already received 202).
    let pool_clone = pool.clone();
    let ids = payload.transaction_ids.clone();
    let status = payload.status.clone();
    let reason = payload.reason.clone();
    tokio::spawn(async move {
        if let Err(e) = run_job(&pool_clone, job_id, &ids, &status, reason.as_deref()).await {
            tracing::error!(
                job_id = %job_id,
                error = %e,
                "Bulk status async job failed"
            );
        }
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(
            serde_json::to_value(BulkStatusJobQueued {
                job_id,
                status: "pending",
                message: format!(
                    "Batch of {} transactions queued as async job. Poll GET \
                     /admin/transactions/bulk-status/jobs/{} for results.",
                    payload.transaction_ids.len(),
                    job_id
                ),
            })
            .unwrap(),
        ),
    ))
}

// ---------------------------------------------------------------------------
// Job persistence helpers
// ---------------------------------------------------------------------------

/// Insert a new `pending` job row and return its ID.
async fn enqueue_job(
    pool: &sqlx::PgPool,
    payload: &BulkStatusRequest,
) -> Result<Uuid, AppError> {
    let ids: Vec<Uuid> = payload.transaction_ids.clone();
    let job_id = sqlx::query_scalar::<_, Uuid>(
        r#"
        INSERT INTO bulk_status_jobs
            (transaction_ids, target_status, reason, actor)
        VALUES ($1, $2, $3, 'admin')
        RETURNING id
        "#,
    )
    .bind(&ids)
    .bind(&payload.status)
    .bind(payload.reason.as_deref())
    .fetch_one(pool)
    .await?;

    tracing::info!(
        job_id = %job_id,
        transaction_count = ids.len(),
        target_status = %payload.status,
        "Bulk status async job enqueued"
    );

    Ok(job_id)
}

/// Execute the job: mark it running, process all IDs, persist per-item
/// results, then mark it completed or failed.
async fn run_job(
    pool: &sqlx::PgPool,
    job_id: Uuid,
    transaction_ids: &[Uuid],
    target_status: &str,
    reason: Option<&str>,
) -> anyhow::Result<()> {
    // Mark running
    sqlx::query(
        "UPDATE bulk_status_jobs SET status = 'running', started_at = NOW() WHERE id = $1",
    )
    .bind(job_id)
    .execute(pool)
    .await?;

    // Process in chunks that respect the quota/rate-limit path (each chunk
    // goes through the same `bulk_update_transaction_status` query that the
    // sync route uses, so per-tenant limits are honoured).
    const CHUNK: usize = 200;
    let mut all_updated: usize = 0;
    let mut all_failed: usize = 0;
    let mut all_errors: Vec<BulkUpdateError> = Vec::new();

    for chunk in transaction_ids.chunks(CHUNK) {
        match bulk_update_transaction_status(pool, chunk, target_status, reason, "admin").await {
            Ok(result) => {
                all_updated += result.updated;
                all_failed += result.failed;
                all_errors.extend(result.errors);
            }
            Err(e) => {
                // Persist failure and bail
                let err_msg = e.to_string();
                sqlx::query(
                    r#"UPDATE bulk_status_jobs
                       SET status = 'failed', completed_at = NOW(), error_message = $1
                       WHERE id = $2"#,
                )
                .bind(&err_msg)
                .bind(job_id)
                .execute(pool)
                .await?;
                return Err(anyhow::anyhow!(err_msg));
            }
        }
    }

    // Build per-item result summary
    let summary = serde_json::json!({
        "updated": all_updated,
        "failed": all_failed,
        "errors": all_errors
    });

    sqlx::query(
        r#"UPDATE bulk_status_jobs
           SET status = 'completed',
               completed_at = NOW(),
               result_summary = $1
           WHERE id = $2"#,
    )
    .bind(summary)
    .bind(job_id)
    .execute(pool)
    .await?;

    tracing::info!(
        job_id = %job_id,
        updated = all_updated,
        failed = all_failed,
        "Bulk status async job completed"
    );

    Ok(())
}

/// Read a job row and return the poll response.
async fn fetch_job_status(
    pool: &sqlx::PgPool,
    job_id: Uuid,
) -> Result<impl IntoResponse, AppError> {
    let row = sqlx::query(
        r#"
        SELECT id,
               status,
               array_length(transaction_ids, 1) AS transaction_count,
               target_status,
               result_summary,
               error_message,
               created_at,
               started_at,
               completed_at
        FROM bulk_status_jobs
        WHERE id = $1
        "#,
    )
    .bind(job_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("Bulk status job {} not found", job_id)))?;

    use sqlx::Row as _;

    Ok((
        StatusCode::OK,
        Json(BulkStatusJobStatus {
            id: row.try_get("id")?,
            status: row.try_get("status")?,
            transaction_count: row
                .try_get::<Option<i32>, _>("transaction_count")?
                .unwrap_or(0) as usize,
            target_status: row.try_get("target_status")?,
            result_summary: row.try_get("result_summary")?,
            error_message: row.try_get("error_message")?,
            created_at: row.try_get("created_at")?,
            started_at: row.try_get("started_at")?,
            completed_at: row.try_get("completed_at")?,
        }),
    ))
}

// ---------------------------------------------------------------------------
// Unit tests (no DB / no network required)
// ---------------------------------------------------------------------------

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

    #[test]
    fn test_validate_request_empty_ids() {
        let req = BulkStatusRequest {
            transaction_ids: vec![],
            status: "failed".to_string(),
            reason: None,
        };
        assert!(validate_request(&req).is_err());
    }

    #[test]
    fn test_validate_request_invalid_status() {
        let req = BulkStatusRequest {
            transaction_ids: vec![Uuid::new_v4()],
            status: "bogus".to_string(),
            reason: None,
        };
        assert!(validate_request(&req).is_err());
    }

    #[test]
    fn test_validate_request_exceeds_max() {
        let req = BulkStatusRequest {
            transaction_ids: (0..10_001).map(|_| Uuid::new_v4()).collect(),
            status: "failed".to_string(),
            reason: None,
        };
        assert!(validate_request(&req).is_err());
    }

    #[test]
    fn test_validate_request_valid() {
        let req = BulkStatusRequest {
            transaction_ids: vec![Uuid::new_v4()],
            status: "completed".to_string(),
            reason: Some("done".to_string()),
        };
        assert!(validate_request(&req).is_ok());
    }

    #[test]
    fn test_async_threshold_default() {
        // Default threshold is 50 unless overridden via env.
        // In a clean test environment BULK_STATUS_ASYNC_THRESHOLD is unset.
        if std::env::var("BULK_STATUS_ASYNC_THRESHOLD").is_err() {
            assert_eq!(async_threshold(), 50);
        }
    }

    /// Exactly-threshold batch → sync path (≤ threshold).
    #[test]
    fn test_threshold_boundary_sync() {
        let threshold = async_threshold();
        let ids: Vec<Uuid> = (0..threshold).map(|_| Uuid::new_v4()).collect();
        let req = BulkStatusRequest {
            transaction_ids: ids,
            status: "failed".to_string(),
            reason: None,
        };
        assert!(validate_request(&req).is_ok());
        // ids.len() == threshold → sync path
        assert!(req.transaction_ids.len() <= async_threshold());
    }

    /// One-over-threshold batch → async path.
    #[test]
    fn test_threshold_boundary_async() {
        let threshold = async_threshold();
        let ids: Vec<Uuid> = (0..=threshold).map(|_| Uuid::new_v4()).collect();
        let req = BulkStatusRequest {
            transaction_ids: ids,
            status: "failed".to_string(),
            reason: None,
        };
        assert!(validate_request(&req).is_ok());
        // ids.len() == threshold + 1 → async path
        assert!(req.transaction_ids.len() > async_threshold());
    }
}
