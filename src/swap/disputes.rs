//! Swap dispute aggregate: state machine plus tenant-scoped persistence.
//!
//! Disputes reference an order id and never mutate order state. Every status
//! transition is written to `audit_logs` in the same transaction. A resolved
//! dispute may be reopened (history lives in the audit log).

use crate::db::audit::AuditLog;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

pub const MAX_TEXT_LEN: usize = 4000;
const ENTITY_TYPE: &str = "swap_dispute";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisputeStatus {
    Open,
    Triaged,
    Resolved,
    Closed,
    Rejected,
}

impl DisputeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Triaged => "triaged",
            Self::Resolved => "resolved",
            Self::Closed => "closed",
            Self::Rejected => "rejected",
        }
    }

    pub fn can_transition_to(self, next: Self) -> bool {
        use DisputeStatus::*;
        matches!(
            (self, next),
            (Open, Triaged)
                | (Open, Rejected)
                | (Triaged, Resolved)
                | (Triaged, Rejected)
                | (Resolved, Closed)
                | (Resolved, Open) // reopen
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DisputeError {
    #[error("invalid transition {0} -> {1}")]
    IllegalTransition(&'static str, &'static str),
    #[error("{0}")]
    Invalid(String),
    #[error("order belongs to a different tenant")]
    TenantMismatch,
    #[error("dispute not found")]
    NotFound,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Dispute {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub order_id: Uuid,
    pub category: String,
    pub reporter: String,
    pub description: String,
    pub status: String,
    pub assignee: Option<String>,
    pub resolution: Option<String>,
    pub adjustment_ref: Option<String>,
    pub sla_deadline: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct NewDispute {
    pub order_id: Uuid,
    pub category: String,
    pub reporter: String,
    pub description: String,
}

#[derive(Debug, Deserialize)]
pub struct DisputeUpdate {
    pub status: DisputeStatus,
    pub assignee: Option<String>,
    pub resolution: Option<String>,
    /// Existing adjustment/refund record this resolution points at.
    pub adjustment_ref: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct DisputeFilter {
    pub order_id: Option<Uuid>,
    pub status: Option<String>,
    /// Only disputes at least this many hours old.
    pub min_age_hours: Option<i64>,
    /// Only disputes past their SLA deadline and still unresolved.
    pub overdue: Option<bool>,
}

fn check_len(field: &str, v: &str) -> Result<(), DisputeError> {
    if v.trim().is_empty() || v.len() > MAX_TEXT_LEN {
        return Err(DisputeError::Invalid(format!(
            "{field} must be 1..={MAX_TEXT_LEN} characters"
        )));
    }
    Ok(())
}

/// Create a dispute. `order_tenant_id` is the owning tenant of the referenced
/// order as resolved by the caller (`None` when the order failed to persist a
/// fill and has no tenant record); a mismatch is rejected.
pub async fn create(
    pool: &PgPool,
    tenant_id: Uuid,
    order_tenant_id: Option<Uuid>,
    new: NewDispute,
    actor: &str,
) -> Result<Dispute, DisputeError> {
    if order_tenant_id.is_some_and(|t| t != tenant_id) {
        return Err(DisputeError::TenantMismatch);
    }
    check_len("description", &new.description)?;
    check_len("reporter", &new.reporter)?;
    if !["settled", "failed", "partial_fill", "other"].contains(&new.category.as_str()) {
        return Err(DisputeError::Invalid("unknown category".into()));
    }
    let mut tx = pool.begin().await?;
    let d: Dispute = sqlx::query_as(
        "INSERT INTO swap_disputes (tenant_id, order_id, category, reporter, description)
         VALUES ($1, $2, $3, $4, $5) RETURNING *",
    )
    .bind(tenant_id)
    .bind(new.order_id)
    .bind(&new.category)
    .bind(&new.reporter)
    .bind(&new.description)
    .fetch_one(&mut *tx)
    .await?;
    AuditLog::log(
        &mut tx,
        d.id,
        ENTITY_TYPE,
        "created",
        None,
        Some(serde_json::json!({"status": "open"})),
        actor,
    )
    .await?;
    tx.commit().await?;
    Ok(d)
}

pub async fn update(
    pool: &PgPool,
    tenant_id: Uuid,
    id: Uuid,
    up: DisputeUpdate,
    actor: &str,
) -> Result<Dispute, DisputeError> {
    for (f, v) in [
        ("resolution", &up.resolution),
        ("assignee", &up.assignee),
        ("adjustment_ref", &up.adjustment_ref),
    ] {
        if let Some(v) = v {
            check_len(f, v)?;
        }
    }
    let mut tx = pool.begin().await?;
    let cur: Dispute =
        sqlx::query_as("SELECT * FROM swap_disputes WHERE id = $1 AND tenant_id = $2 FOR UPDATE")
            .bind(id)
            .bind(tenant_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(DisputeError::NotFound)?;
    let from = parse_status(&cur.status);
    if !from.can_transition_to(up.status) {
        return Err(DisputeError::IllegalTransition(
            from.as_str(),
            up.status.as_str(),
        ));
    }
    if up.status == DisputeStatus::Resolved && up.resolution.is_none() {
        return Err(DisputeError::Invalid(
            "resolution required to resolve".into(),
        ));
    }
    let d: Dispute = sqlx::query_as(
        "UPDATE swap_disputes SET status = $3, assignee = COALESCE($4, assignee),
             resolution = COALESCE($5, resolution), adjustment_ref = COALESCE($6, adjustment_ref),
             updated_at = NOW()
         WHERE id = $1 AND tenant_id = $2 RETURNING *",
    )
    .bind(id)
    .bind(tenant_id)
    .bind(up.status.as_str())
    .bind(&up.assignee)
    .bind(&up.resolution)
    .bind(&up.adjustment_ref)
    .fetch_one(&mut *tx)
    .await?;
    AuditLog::log(
        &mut tx,
        id,
        ENTITY_TYPE,
        "status_change",
        Some(serde_json::json!({"status": from.as_str()})),
        Some(serde_json::json!({
            "status": up.status.as_str(),
            "adjustment_ref": up.adjustment_ref,
            "assignee": up.assignee,
        })),
        actor,
    )
    .await?;
    tx.commit().await?;
    Ok(d)
}

pub async fn list(
    pool: &PgPool,
    tenant_id: Uuid,
    f: &DisputeFilter,
) -> Result<Vec<Dispute>, DisputeError> {
    Ok(sqlx::query_as(
        "SELECT * FROM swap_disputes
         WHERE tenant_id = $1
           AND ($2::uuid IS NULL OR order_id = $2)
           AND ($3::text IS NULL OR status = $3)
           AND ($4::bigint IS NULL OR created_at <= NOW() - make_interval(hours => $4::int))
           AND (NOT COALESCE($5, false) OR (status IN ('open','triaged') AND sla_deadline < NOW()))
         ORDER BY created_at DESC LIMIT 200",
    )
    .bind(tenant_id)
    .bind(f.order_id)
    .bind(&f.status)
    .bind(f.min_age_hours)
    .bind(f.overdue)
    .fetch_all(pool)
    .await?)
}

fn parse_status(s: &str) -> DisputeStatus {
    match s {
        "triaged" => DisputeStatus::Triaged,
        "resolved" => DisputeStatus::Resolved,
        "closed" => DisputeStatus::Closed,
        "rejected" => DisputeStatus::Rejected,
        _ => DisputeStatus::Open,
    }
}

#[cfg(test)]
mod tests {
    use super::DisputeStatus::*;

    #[test]
    fn legal_transitions() {
        assert!(Open.can_transition_to(Triaged));
        assert!(Triaged.can_transition_to(Resolved));
        assert!(Resolved.can_transition_to(Closed));
        assert!(Resolved.can_transition_to(Open));
        assert!(Open.can_transition_to(Rejected));
    }

    #[test]
    fn illegal_transitions() {
        assert!(!Open.can_transition_to(Resolved));
        assert!(!Closed.can_transition_to(Open));
        assert!(!Rejected.can_transition_to(Triaged));
        assert!(!Triaged.can_transition_to(Triaged));
    }
}
