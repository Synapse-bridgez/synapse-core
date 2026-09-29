use crate::AppState;
use sqlx::{PgPool, Row};
use uuid::Uuid;
use chrono::{DateTime, Utc};
use serde::{Serialize, Deserialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct TenantExportJob {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub job_status: String,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub requested_by_admin: Uuid,
    pub archive_location: Option<String>,
    pub archive_size_bytes: Option<i64>,
    pub error_message: Option<String>,
    pub export_scope: String,
    pub row_count_transactions: i64,
    pub row_count_settlements: i64,
    pub row_count_audit_logs: i64,
    pub row_count_webhook_events: i64,
    pub retention_days: i32,
    pub delete_after: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct ExportStats {
    pub total_transactions: i64,
    pub total_settlements: i64,
    pub total_audit_logs: i64,
    pub total_webhook_events: i64,
}

/// Creates a new tenant data export job.
/// Returns the job ID for status tracking.
pub async fn create_export_job(
    pool: &PgPool,
    tenant_id: Uuid,
    admin_id: Uuid,
    retention_days: Option<i32>,
) -> Result<TenantExportJob, sqlx::Error> {
    let retention = retention_days.unwrap_or(30);
    let delete_after = Utc::now() + chrono::Duration::days(retention as i64);

    sqlx::query_as::<_, TenantExportJob>(
        r#"
        INSERT INTO tenant_data_export_jobs
        (tenant_id, requested_by_admin, job_status, export_scope, retention_days, delete_after)
        VALUES ($1, $2, 'pending', 'full', $3, $4)
        RETURNING id, tenant_id, job_status, created_at, started_at, completed_at,
                  requested_by_admin, archive_location, archive_size_bytes, error_message,
                  export_scope, row_count_transactions, row_count_settlements,
                  row_count_audit_logs, row_count_webhook_events, retention_days, delete_after
        "#,
    )
    .bind(tenant_id)
    .bind(admin_id)
    .bind(retention)
    .bind(delete_after)
    .fetch_one(pool)
    .await
}

/// Retrieves the current status of an export job.
pub async fn get_export_job(
    pool: &PgPool,
    job_id: Uuid,
) -> Result<Option<TenantExportJob>, sqlx::Error> {
    sqlx::query_as::<_, TenantExportJob>(
        r#"
        SELECT id, tenant_id, job_status, created_at, started_at, completed_at,
               requested_by_admin, archive_location, archive_size_bytes, error_message,
               export_scope, row_count_transactions, row_count_settlements,
               row_count_audit_logs, row_count_webhook_events, retention_days, delete_after
        FROM tenant_data_export_jobs
        WHERE id = $1
        "#,
    )
    .bind(job_id)
    .fetch_optional(pool)
    .await
}

/// Marks an export job as in progress and sets the start time.
pub async fn start_export_job(
    pool: &PgPool,
    job_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE tenant_data_export_jobs
        SET job_status = 'in_progress', started_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(job_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Marks an export job as completed with archive details.
pub async fn complete_export_job(
    pool: &PgPool,
    job_id: Uuid,
    archive_location: &str,
    archive_size_bytes: i64,
    stats: &ExportStats,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE tenant_data_export_jobs
        SET job_status = 'completed',
            completed_at = NOW(),
            archive_location = $2,
            archive_size_bytes = $3,
            row_count_transactions = $4,
            row_count_settlements = $5,
            row_count_audit_logs = $6,
            row_count_webhook_events = $7
        WHERE id = $1
        "#,
    )
    .bind(job_id)
    .bind(archive_location)
    .bind(archive_size_bytes)
    .bind(stats.total_transactions)
    .bind(stats.total_settlements)
    .bind(stats.total_audit_logs)
    .bind(stats.total_webhook_events)
    .execute(pool)
    .await?;
    Ok(())
}

/// Marks an export job as failed with an error message.
pub async fn fail_export_job(
    pool: &PgPool,
    job_id: Uuid,
    error_message: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE tenant_data_export_jobs
        SET job_status = 'failed',
            completed_at = NOW(),
            error_message = $2
        WHERE id = $1
        "#,
    )
    .bind(job_id)
    .bind(error_message)
    .execute(pool)
    .await?;
    Ok(())
}

/// Counts transactions for a tenant (respects RLS).
pub async fn count_tenant_transactions(
    pool: &PgPool,
    tenant_id: Uuid,
) -> Result<i64, sqlx::Error> {
    let row = sqlx::query(
        "SELECT COUNT(*) as count FROM transactions WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_one(pool)
    .await?;

    Ok(row.get::<i64, _>("count"))
}

/// Counts settlements for a tenant (respects RLS).
pub async fn count_tenant_settlements(
    pool: &PgPool,
    tenant_id: Uuid,
) -> Result<i64, sqlx::Error> {
    let row = sqlx::query(
        "SELECT COUNT(*) as count FROM settlements WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_one(pool)
    .await?;

    Ok(row.get::<i64, _>("count"))
}

/// Counts audit logs for a tenant (respects RLS).
pub async fn count_tenant_audit_logs(
    pool: &PgPool,
    tenant_id: Uuid,
) -> Result<i64, sqlx::Error> {
    let row = sqlx::query(
        "SELECT COUNT(*) as count FROM audit_logs WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_one(pool)
    .await?;

    Ok(row.get::<i64, _>("count"))
}

/// Counts webhook events for a tenant (respects RLS).
pub async fn count_tenant_webhook_events(
    pool: &PgPool,
    tenant_id: Uuid,
) -> Result<i64, sqlx::Error> {
    let row = sqlx::query(
        "SELECT COUNT(*) as count FROM webhook_events WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_one(pool)
    .await?;

    Ok(row.get::<i64, _>("count"))
}

/// Retrieves all pending export jobs (for async job workers).
pub async fn get_pending_export_jobs(
    pool: &PgPool,
) -> Result<Vec<TenantExportJob>, sqlx::Error> {
    sqlx::query_as::<_, TenantExportJob>(
        r#"
        SELECT id, tenant_id, job_status, created_at, started_at, completed_at,
               requested_by_admin, archive_location, archive_size_bytes, error_message,
               export_scope, row_count_transactions, row_count_settlements,
               row_count_audit_logs, row_count_webhook_events, retention_days, delete_after
        FROM tenant_data_export_jobs
        WHERE job_status = 'pending'
        ORDER BY created_at ASC
        LIMIT 10
        "#,
    )
    .fetch_all(pool)
    .await
}

/// Cleans up expired export archives (older than retention period).
pub async fn cleanup_expired_exports(
    pool: &PgPool,
) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        "DELETE FROM tenant_data_export_jobs WHERE delete_after < NOW() AND job_status = 'completed'",
    )
    .execute(pool)
    .await?;

    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_export_job_creation() {
        let tenant_id = Uuid::new_v4();
        let admin_id = Uuid::new_v4();

        // This test would require a database connection in a real scenario
        // For now, we just verify the function signatures compile
        let _ = (tenant_id, admin_id);
    }

    #[test]
    fn test_export_stats() {
        let stats = ExportStats {
            total_transactions: 1000,
            total_settlements: 500,
            total_audit_logs: 2000,
            total_webhook_events: 300,
        };

        assert_eq!(stats.total_transactions, 1000);
        assert_eq!(stats.total_settlements, 500);
    }
}
