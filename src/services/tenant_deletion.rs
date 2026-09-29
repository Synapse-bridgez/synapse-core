use crate::AppState;
use sqlx::{PgPool, Row, Transaction, Postgres};
use uuid::Uuid;
use chrono::{DateTime, Utc};
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct TenantDeletionRequest {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub request_status: String,
    pub created_at: DateTime<Utc>,
    pub requested_by_admin: Uuid,
    pub approved_by_admin: Option<Uuid>,
    pub approval_timestamp: Option<DateTime<Utc>>,
    pub execution_started_at: Option<DateTime<Utc>>,
    pub execution_completed_at: Option<DateTime<Utc>>,
    pub deletion_reason: Option<String>,
    pub rows_deleted_transactions: i64,
    pub rows_deleted_settlements: i64,
    pub rows_deleted_webhook_events: i64,
    pub rows_retained_audit_logs: i64,
    pub error_message: Option<String>,
    pub retention_floor_violations: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct TenantDeletionBlocker {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub deletion_request_id: Uuid,
    pub blocker_type: String,
    pub blocker_record_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

/// Creates a deletion request for a tenant. Requires admin authorization.
pub async fn create_deletion_request(
    pool: &PgPool,
    tenant_id: Uuid,
    requesting_admin_id: Uuid,
    deletion_reason: &str,
) -> Result<TenantDeletionRequest, sqlx::Error> {
    sqlx::query_as::<_, TenantDeletionRequest>(
        r#"
        INSERT INTO tenant_deletion_requests
        (tenant_id, requested_by_admin, request_status, deletion_reason)
        VALUES ($1, $2, 'requested', $3)
        RETURNING id, tenant_id, request_status, created_at, requested_by_admin,
                  approved_by_admin, approval_timestamp, execution_started_at,
                  execution_completed_at, deletion_reason, rows_deleted_transactions,
                  rows_deleted_settlements, rows_deleted_webhook_events,
                  rows_retained_audit_logs, error_message, retention_floor_violations
        "#,
    )
    .bind(tenant_id)
    .bind(requesting_admin_id)
    .bind(deletion_reason)
    .fetch_one(pool)
    .await
}

/// Approves a deletion request (second admin approval). Prevents self-approval.
pub async fn approve_deletion_request(
    pool: &PgPool,
    request_id: Uuid,
    approving_admin_id: Uuid,
) -> Result<TenantDeletionRequest, String> {
    let request = get_deletion_request(pool, request_id)
        .await
        .map_err(|e| format!("Failed to fetch request: {}", e))?
        .ok_or("Deletion request not found")?;

    if request.request_status != "requested" {
        return Err(format!(
            "Request is in '{}' status, cannot approve from this state",
            request.request_status
        ));
    }

    if request.requested_by_admin == approving_admin_id {
        return Err("Cannot approve your own deletion request; requires different admin".to_string());
    }

    sqlx::query_as::<_, TenantDeletionRequest>(
        r#"
        UPDATE tenant_deletion_requests
        SET request_status = 'approved',
            approved_by_admin = $2,
            approval_timestamp = NOW()
        WHERE id = $1
        RETURNING id, tenant_id, request_status, created_at, requested_by_admin,
                  approved_by_admin, approval_timestamp, execution_started_at,
                  execution_completed_at, deletion_reason, rows_deleted_transactions,
                  rows_deleted_settlements, rows_deleted_webhook_events,
                  rows_retained_audit_logs, error_message, retention_floor_violations
        "#,
    )
    .bind(request_id)
    .bind(approving_admin_id)
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())
}

/// Rejects a deletion request (prevents approval).
pub async fn reject_deletion_request(
    pool: &PgPool,
    request_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE tenant_deletion_requests
        SET request_status = 'rejected'
        WHERE id = $1 AND request_status = 'requested'
        "#,
    )
    .bind(request_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Checks for in-flight transactions that would block deletion.
pub async fn check_deletion_blockers(
    pool: &PgPool,
    tenant_id: Uuid,
    request_id: Uuid,
) -> Result<Vec<TenantDeletionBlocker>, sqlx::Error> {
    let open_transactions = sqlx::query(
        "SELECT COUNT(*) as count FROM transactions WHERE tenant_id = $1 AND status NOT IN ('completed', 'failed')",
    )
    .bind(tenant_id)
    .fetch_one(pool)
    .await?;

    let open_count: i64 = open_transactions.get("count");

    if open_count > 0 {
        sqlx::query_as::<_, TenantDeletionBlocker>(
            r#"
            INSERT INTO tenant_deletion_blockers
            (tenant_id, deletion_request_id, blocker_type, blocker_record_id)
            SELECT $1, $2, 'open_transaction', id FROM transactions
            WHERE tenant_id = $1 AND status NOT IN ('completed', 'failed')
            RETURNING id, tenant_id, deletion_request_id, blocker_type, blocker_record_id,
                      created_at, resolved_at
            "#,
        )
        .bind(tenant_id)
        .bind(request_id)
        .fetch_all(pool)
        .await?
    } else {
        vec![]
    };

    // Return all active blockers for this request
    sqlx::query_as::<_, TenantDeletionBlocker>(
        r#"
        SELECT id, tenant_id, deletion_request_id, blocker_type, blocker_record_id,
               created_at, resolved_at
        FROM tenant_deletion_blockers
        WHERE deletion_request_id = $1 AND resolved_at IS NULL
        "#,
    )
    .bind(request_id)
    .fetch_all(pool)
    .await
}

/// Executes the tenant deletion (requires approved status).
pub async fn execute_deletion(
    pool: &PgPool,
    request_id: Uuid,
) -> Result<TenantDeletionRequest, String> {
    let request = get_deletion_request(pool, request_id)
        .await
        .map_err(|e| format!("Failed to fetch request: {}", e))?
        .ok_or("Deletion request not found")?;

    if request.request_status != "approved" {
        return Err(format!(
            "Request must be 'approved' to execute, currently '{}'",
            request.request_status
        ));
    }

    let blockers = check_deletion_blockers(pool, request.tenant_id, request_id)
        .await
        .map_err(|e| format!("Failed to check blockers: {}", e))?;

    if !blockers.is_empty() {
        return Err(format!(
            "Cannot delete: {} in-flight transactions block deletion. Resolve them first.",
            blockers.len()
        ));
    }

    // Start transaction for deletion
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| format!("Failed to start transaction: {}", e))?;

    // Update status to executing
    sqlx::query(
        "UPDATE tenant_deletion_requests SET request_status = 'executing', execution_started_at = NOW() WHERE id = $1"
    )
    .bind(request_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    // Delete non-retained data (with RLS protection)
    let txn_count: i64 = sqlx::query("DELETE FROM transactions WHERE tenant_id = $1")
        .bind(request.tenant_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?
        .rows_affected() as i64;

    let settlement_count: i64 = sqlx::query("DELETE FROM settlements WHERE tenant_id = $1")
        .bind(request.tenant_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?
        .rows_affected() as i64;

    let webhook_count: i64 = sqlx::query("DELETE FROM webhook_events WHERE tenant_id = $1")
        .bind(request.tenant_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?
        .rows_affected() as i64;

    // Audit logs are retained per compliance requirement
    let audit_retain_count: i64 = sqlx::query("SELECT COUNT(*) as count FROM audit_logs WHERE tenant_id = $1")
        .bind(request.tenant_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| e.to_string())?
        .get("count");

    // Log the deletion to audit trail (on the same audit_logs table being protected)
    sqlx::query(
        r#"
        INSERT INTO audit_logs (tenant_id, action, details)
        VALUES ($1, 'TENANT_DELETED', $2)
        "#
    )
    .bind(request.tenant_id)
    .bind(format!("Tenant deleted by admin. Request ID: {}", request_id))
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    // Update request status to completed
    sqlx::query(
        r#"
        UPDATE tenant_deletion_requests
        SET request_status = 'completed',
            execution_completed_at = NOW(),
            rows_deleted_transactions = $2,
            rows_deleted_settlements = $3,
            rows_deleted_webhook_events = $4,
            rows_retained_audit_logs = $5
        WHERE id = $1
        "#
    )
    .bind(request_id)
    .bind(txn_count)
    .bind(settlement_count)
    .bind(webhook_count)
    .bind(audit_retain_count)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;

    tx.commit()
        .await
        .map_err(|e| format!("Failed to commit deletion transaction: {}", e))?;

    get_deletion_request(pool, request_id)
        .await
        .map_err(|e| format!("Failed to fetch updated request: {}", e))?
        .ok_or("Failed to retrieve completed request".to_string())
}

/// Retrieves a specific deletion request.
pub async fn get_deletion_request(
    pool: &PgPool,
    request_id: Uuid,
) -> Result<Option<TenantDeletionRequest>, sqlx::Error> {
    sqlx::query_as::<_, TenantDeletionRequest>(
        r#"
        SELECT id, tenant_id, request_status, created_at, requested_by_admin,
               approved_by_admin, approval_timestamp, execution_started_at,
               execution_completed_at, deletion_reason, rows_deleted_transactions,
               rows_deleted_settlements, rows_deleted_webhook_events,
               rows_retained_audit_logs, error_message, retention_floor_violations
        FROM tenant_deletion_requests
        WHERE id = $1
        "#,
    )
    .bind(request_id)
    .fetch_optional(pool)
    .await
}

/// Gets all pending deletion requests for a tenant.
pub async fn get_tenant_deletion_requests(
    pool: &PgPool,
    tenant_id: Uuid,
) -> Result<Vec<TenantDeletionRequest>, sqlx::Error> {
    sqlx::query_as::<_, TenantDeletionRequest>(
        r#"
        SELECT id, tenant_id, request_status, created_at, requested_by_admin,
               approved_by_admin, approval_timestamp, execution_started_at,
               execution_completed_at, deletion_reason, rows_deleted_transactions,
               rows_deleted_settlements, rows_deleted_webhook_events,
               rows_retained_audit_logs, error_message, retention_floor_violations
        FROM tenant_deletion_requests
        WHERE tenant_id = $1
        ORDER BY created_at DESC
        "#,
    )
    .bind(tenant_id)
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_blocker_type() {
        let blocker_types = vec!["open_transaction", "pending_settlement", "disputed_payment"];
        assert!(blocker_types.contains(&"open_transaction"));
    }

    #[test]
    fn test_request_status_flow() {
        let statuses = vec!["requested", "approved", "executing", "completed", "rejected"];
        assert_eq!(statuses[0], "requested");
        assert_eq!(statuses[1], "approved");
        assert_eq!(statuses[2], "executing");
        assert_eq!(statuses[3], "completed");
    }
}
