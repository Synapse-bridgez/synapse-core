//! Admin endpoints for point-in-time-recovery (PITR) backup restores.
//!
//! Both routes sit behind the `admin_auth` layer applied to `admin_router`
//! in `src/lib.rs`. Submitting a restore never blocks on the actual restore
//! work — see [`crate::services::pitr`] for the async job machinery.

use crate::error::AppError;
use crate::services::pitr::{PitrService, ShellPitrExecutor};
use crate::ApiState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::sync::Arc;
use uuid::Uuid;

pub fn backup_routes() -> Router<ApiState> {
    Router::new()
        .route("/restore-pitr", post(submit_restore_pitr))
        .route("/restore-pitr/:job_id", get(get_restore_pitr_job))
}

#[derive(Debug, Deserialize)]
pub struct RestorePitrRequest {
    /// The point in time to restore to.
    pub target_timestamp: DateTime<Utc>,
    /// When true, only validates the target and records the attempt —
    /// no restore is performed. Required client-side confirmation (e.g. a
    /// CLI `--yes` flag) should gate ever sending `dry_run: false`.
    #[serde(default)]
    pub dry_run: bool,
    /// Identity of the operator requesting the restore, recorded in the
    /// audit log. Falls back to `"admin"` when not provided.
    pub requested_by: Option<String>,
}

fn pitr_service(state: &ApiState) -> PitrService {
    let executor = Arc::new(ShellPitrExecutor::from_env());
    PitrService::new(state.app_state.db.clone(), executor)
}

/// POST /admin/backup/restore-pitr
///
/// Validates `target_timestamp` against available WAL/backup coverage and,
/// unless `dry_run` is set, kicks off the restore as a background job.
/// Returns the created job (id + status) immediately; poll
/// `GET /admin/backup/restore-pitr/:job_id` for progress.
pub async fn submit_restore_pitr(
    State(state): State<ApiState>,
    Json(req): Json<RestorePitrRequest>,
) -> Result<impl IntoResponse, AppError> {
    let service = pitr_service(&state);
    let actor = req.requested_by.as_deref().unwrap_or("admin");

    match service
        .submit_restore(req.target_timestamp, actor, req.dry_run)
        .await
    {
        Ok(job) => Ok((StatusCode::ACCEPTED, Json(job))),
        Err(crate::services::pitr::PitrError::InvalidTarget(msg)) => Err(AppError::BadRequest(msg)),
        Err(crate::services::pitr::PitrError::Database(e)) => {
            Err(AppError::DatabaseError(e.to_string()))
        }
    }
}

/// GET /admin/backup/restore-pitr/:job_id
///
/// Returns the current status of a previously submitted restore job.
pub async fn get_restore_pitr_job(
    State(state): State<ApiState>,
    Path(job_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let service = pitr_service(&state);

    let job = service
        .get_job(job_id)
        .await
        .map_err(|e| AppError::DatabaseError(e.to_string()))?
        .ok_or_else(|| AppError::NotFound(format!("restore job {job_id} not found")))?;

    Ok((StatusCode::OK, Json(job)))
}
