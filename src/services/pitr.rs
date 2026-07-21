//! Point-in-time-recovery (PITR) restore orchestration.
//!
//! A PITR restore is long-running and destructive, so it is modelled as an
//! async job persisted in `pitr_restore_jobs`: submitting a restore inserts a
//! row and (for real, non-dry-run restores) spawns a background task that
//! drives the row through `pending -> running -> succeeded|failed`. Callers
//! poll [`PitrService::get_job`] for progress instead of blocking on the
//! request thread.
//!
//! Target-timestamp validity is checked against the coverage window implied
//! by `backup_verification_logs`: the earliest and latest `latest_timestamp`
//! among rows with `verification_status = 'verified'` bound the range of
//! points we can confidently recover to. A target outside that window (or a
//! target when no backup has ever been verified) is rejected before any job
//! is spawned.

use crate::db::audit::{AuditLog, ENTITY_BACKUP};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use sqlx::PgPool;
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

pub const STATUS_PENDING: &str = "pending";
pub const STATUS_RUNNING: &str = "running";
pub const STATUS_SUCCEEDED: &str = "succeeded";
pub const STATUS_FAILED: &str = "failed";

#[derive(Debug, thiserror::Error)]
pub enum PitrError {
    #[error("{0}")]
    InvalidTarget(String),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

/// The window of timestamps we can currently restore to, derived from
/// verified backups.
#[derive(Debug, Clone, Copy)]
pub struct PitrCoverage {
    pub earliest: DateTime<Utc>,
    pub latest: DateTime<Utc>,
}

/// Compute the current PITR coverage window from `backup_verification_logs`.
/// Returns `None` when no backup has ever been successfully verified.
pub async fn get_pitr_coverage(pool: &PgPool) -> Result<Option<PitrCoverage>, sqlx::Error> {
    let row: (Option<DateTime<Utc>>, Option<DateTime<Utc>>) = sqlx::query_as(
        "SELECT MIN(latest_timestamp), MAX(latest_timestamp) FROM backup_verification_logs \
         WHERE verification_status = 'verified' AND latest_timestamp IS NOT NULL",
    )
    .fetch_one(pool)
    .await?;

    Ok(match row {
        (Some(earliest), Some(latest)) => Some(PitrCoverage { earliest, latest }),
        _ => None,
    })
}

/// Validate a target restore timestamp against the current coverage window.
/// Returns a specific, actionable error message on rejection rather than
/// silently clamping to the nearest available point.
pub fn validate_target_timestamp(
    target: DateTime<Utc>,
    now: DateTime<Utc>,
    coverage: Option<&PitrCoverage>,
) -> Result<(), String> {
    if target > now {
        return Err(format!(
            "target timestamp {target} is in the future; the latest possible recovery point is now ({now})"
        ));
    }

    let coverage = coverage.ok_or_else(|| {
        "no verified backups are available for point-in-time recovery; run backup verification \
         before attempting a restore"
            .to_string()
    })?;

    if target < coverage.earliest {
        return Err(format!(
            "target timestamp {target} is before the earliest available recovery point \
             ({}); the oldest verified backup only covers data from that point onward",
            coverage.earliest
        ));
    }

    if target > coverage.latest {
        return Err(format!(
            "target timestamp {target} is after the latest available recovery point \
             ({}); WAL/backup coverage does not yet extend that far",
            coverage.latest
        ));
    }

    Ok(())
}

/// A persisted PITR restore attempt.
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct RestoreJob {
    pub id: Uuid,
    pub target_timestamp: DateTime<Utc>,
    pub status: String,
    pub dry_run: bool,
    pub requested_by: String,
    pub detail: Option<String>,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

const RESTORE_JOB_COLUMNS: &str = "id, target_timestamp, status, dry_run, requested_by, \
     detail, error_message, created_at, started_at, completed_at";

/// Performs the actual restore mechanics. Kept as a trait so the async job
/// machinery (validation, persistence, status polling, audit logging) can be
/// exercised in tests without invoking real `pg_basebackup`/WAL-replay
/// tooling, which needs a dedicated environment (see `scripts/pitr_restore.sh`).
#[async_trait]
pub trait PitrExecutor: Send + Sync {
    /// Perform the restore to `target_timestamp`. Returns a human-readable
    /// detail message on success, or a human-readable error on failure.
    async fn restore(&self, target_timestamp: DateTime<Utc>) -> Result<String, String>;
}

/// Default executor: shells out to an external restore script. Real
/// Postgres PITR (pg_basebackup + WAL replay + recovery_target_time) needs
/// server-side binaries and access to the archived WAL, which live outside
/// this process's container — see `scripts/pitr_restore.sh` for the
/// documented contract the script is expected to fulfil.
pub struct ShellPitrExecutor {
    script: PathBuf,
    database_url: String,
    workspace_dir: PathBuf,
    wal_archive_dir: PathBuf,
}

impl ShellPitrExecutor {
    /// Builds an executor from environment configuration. The restore script
    /// runs as a separate process, so it needs its own explicit connection
    /// string rather than reusing the app's already-resolved `PgPool` —
    /// `PITR_DATABASE_URL` takes precedence, falling back to `DATABASE_URL`
    /// (matches how the app itself resolves its primary connection string
    /// outside of Vault-based secret injection).
    pub fn from_env() -> Self {
        let database_url = std::env::var("PITR_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .unwrap_or_default();

        Self {
            script: std::env::var("PITR_RESTORE_SCRIPT")
                .unwrap_or_else(|_| "scripts/pitr_restore.sh".to_string())
                .into(),
            database_url,
            workspace_dir: std::env::var("PITR_WORKSPACE_DIR")
                .unwrap_or_else(|_| "./pitr_workspace".to_string())
                .into(),
            wal_archive_dir: std::env::var("WAL_ARCHIVE_DIR")
                .unwrap_or_else(|_| "/var/lib/postgresql/wal_archive".to_string())
                .into(),
        }
    }
}

#[async_trait]
impl PitrExecutor for ShellPitrExecutor {
    async fn restore(&self, target_timestamp: DateTime<Utc>) -> Result<String, String> {
        let output = tokio::process::Command::new(&self.script)
            .arg(target_timestamp.to_rfc3339())
            .arg(&self.database_url)
            .arg(&self.workspace_dir)
            .arg(&self.wal_archive_dir)
            .output()
            .await
            .map_err(|e| {
                format!(
                    "failed to launch PITR restore script {}: {e}",
                    self.script.display()
                )
            })?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if stderr.is_empty() {
                Err(format!(
                    "PITR restore script exited with status {}",
                    output.status
                ))
            } else {
                Err(stderr)
            }
        }
    }
}

pub struct PitrService {
    pool: PgPool,
    executor: Arc<dyn PitrExecutor>,
}

impl PitrService {
    pub fn new(pool: PgPool, executor: Arc<dyn PitrExecutor>) -> Self {
        Self { pool, executor }
    }

    /// Validate and record a restore request. For `dry_run` requests the job
    /// resolves immediately with no destructive action taken. For real
    /// restores a background task is spawned so this call returns as soon as
    /// the job is persisted — the caller polls [`Self::get_job`] for
    /// completion.
    pub async fn submit_restore(
        &self,
        target: DateTime<Utc>,
        requested_by: &str,
        dry_run: bool,
    ) -> Result<RestoreJob, PitrError> {
        let coverage = get_pitr_coverage(&self.pool).await?;
        let now = Utc::now();
        let validation = validate_target_timestamp(target, now, coverage.as_ref());

        let initial_status = if validation.is_err() {
            STATUS_FAILED
        } else {
            STATUS_PENDING
        };
        let rejection_reason = validation.as_ref().err().cloned();

        let mut tx = self.pool.begin().await?;
        let job: RestoreJob = sqlx::query_as(&format!(
            "INSERT INTO pitr_restore_jobs \
             (target_timestamp, status, dry_run, requested_by, error_message, completed_at) \
             VALUES ($1, $2, $3, $4, $5, CASE WHEN $2 = '{STATUS_FAILED}' THEN NOW() ELSE NULL END) \
             RETURNING {RESTORE_JOB_COLUMNS}"
        ))
        .bind(target)
        .bind(initial_status)
        .bind(dry_run)
        .bind(requested_by)
        .bind(rejection_reason.as_deref())
        .fetch_one(&mut *tx)
        .await?;

        AuditLog::log(
            &mut tx,
            job.id,
            ENTITY_BACKUP,
            if validation.is_err() {
                "pitr_restore_rejected"
            } else {
                "pitr_restore_submitted"
            },
            None,
            Some(json!({
                "target_timestamp": target,
                "dry_run": dry_run,
                "reason": rejection_reason,
            })),
            requested_by,
        )
        .await?;

        tx.commit().await?;

        if let Err(msg) = validation {
            return Err(PitrError::InvalidTarget(msg));
        }

        if dry_run {
            let coverage = coverage.expect("validated target implies coverage exists");
            let detail = format!(
                "dry run: target timestamp {target} is within available recovery coverage \
                 [{}, {}]; no restore was performed",
                coverage.earliest, coverage.latest
            );
            Self::finish_job(
                &self.pool,
                job.id,
                STATUS_SUCCEEDED,
                Some(&detail),
                None,
                requested_by,
            )
            .await?;
            return Ok(self
                .get_job(job.id)
                .await?
                .expect("job was just inserted and finished"));
        }

        let pool = self.pool.clone();
        let executor = self.executor.clone();
        let job_id = job.id;
        let actor = requested_by.to_string();
        tokio::spawn(async move {
            if sqlx::query(
                "UPDATE pitr_restore_jobs SET status = $1, started_at = NOW() WHERE id = $2",
            )
            .bind(STATUS_RUNNING)
            .bind(job_id)
            .execute(&pool)
            .await
            .is_err()
            {
                tracing::error!(job_id = %job_id, "failed to mark PITR restore job as running");
                return;
            }

            let result = executor.restore(target).await;
            let (status, detail, error) = match &result {
                Ok(detail) => (STATUS_SUCCEEDED, Some(detail.as_str()), None),
                Err(err) => (STATUS_FAILED, None, Some(err.as_str())),
            };

            if let Err(e) = Self::finish_job(&pool, job_id, status, detail, error, &actor).await {
                tracing::error!(job_id = %job_id, error = %e, "failed to record PITR restore job completion");
            }
        });

        Ok(job)
    }

    pub async fn get_job(&self, id: Uuid) -> Result<Option<RestoreJob>, sqlx::Error> {
        sqlx::query_as(&format!(
            "SELECT {RESTORE_JOB_COLUMNS} FROM pitr_restore_jobs WHERE id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await
    }

    async fn finish_job(
        pool: &PgPool,
        job_id: Uuid,
        status: &str,
        detail: Option<&str>,
        error_message: Option<&str>,
        actor: &str,
    ) -> Result<(), sqlx::Error> {
        let mut tx = pool.begin().await?;

        sqlx::query(
            "UPDATE pitr_restore_jobs SET status = $1, detail = $2, error_message = $3, \
             completed_at = NOW() WHERE id = $4",
        )
        .bind(status)
        .bind(detail)
        .bind(error_message)
        .bind(job_id)
        .execute(&mut *tx)
        .await?;

        AuditLog::log(
            &mut tx,
            job_id,
            ENTITY_BACKUP,
            "pitr_restore_completed",
            None,
            Some(json!({
                "status": status,
                "detail": detail,
                "error": error_message,
            })),
            actor,
        )
        .await?;

        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    fn ts(y: i32, m: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap()
    }

    #[test]
    fn test_validate_rejects_future_timestamp() {
        let now = ts(2026, 6, 1);
        let target = now + Duration::days(1);
        let err = validate_target_timestamp(target, now, None).unwrap_err();
        assert!(err.contains("in the future"), "unexpected error: {err}");
    }

    #[test]
    fn test_validate_rejects_when_no_coverage() {
        let now = ts(2026, 6, 1);
        let err = validate_target_timestamp(ts(2026, 5, 1), now, None).unwrap_err();
        assert!(
            err.contains("no verified backups"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_validate_rejects_before_earliest() {
        let now = ts(2026, 6, 1);
        let coverage = PitrCoverage {
            earliest: ts(2026, 5, 1),
            latest: ts(2026, 5, 20),
        };
        let err = validate_target_timestamp(ts(2026, 4, 1), now, Some(&coverage)).unwrap_err();
        assert!(
            err.contains("before the earliest available recovery point"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_validate_rejects_after_latest() {
        let now = ts(2026, 6, 1);
        let coverage = PitrCoverage {
            earliest: ts(2026, 5, 1),
            latest: ts(2026, 5, 20),
        };
        let err = validate_target_timestamp(ts(2026, 5, 25), now, Some(&coverage)).unwrap_err();
        assert!(
            err.contains("after the latest available recovery point"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn test_validate_accepts_target_within_coverage() {
        let now = ts(2026, 6, 1);
        let coverage = PitrCoverage {
            earliest: ts(2026, 5, 1),
            latest: ts(2026, 5, 20),
        };
        assert!(validate_target_timestamp(ts(2026, 5, 10), now, Some(&coverage)).is_ok());
    }

    #[test]
    fn test_validate_accepts_coverage_boundaries_inclusive() {
        let now = ts(2026, 6, 1);
        let coverage = PitrCoverage {
            earliest: ts(2026, 5, 1),
            latest: ts(2026, 5, 20),
        };
        assert!(validate_target_timestamp(coverage.earliest, now, Some(&coverage)).is_ok());
        assert!(validate_target_timestamp(coverage.latest, now, Some(&coverage)).is_ok());
    }
}
