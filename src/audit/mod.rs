//! Operator audit logging for administrative swap actions (#1518).
//!
//! Every administrative mutation of the swap engine - kill-switch toggles,
//! circuit-breaker overrides, manual order intervention, configuration changes,
//! replay/backfill triggers, and limit adjustments - must go through
//! [`dispatch`]. It writes one hash-chained record to `operator_audit_log` in
//! the **same transaction** as the action, so an action never commits without
//! its audit record and an audit record never exists for a rolled-back action.
//!
//! Failure policy: **fail closed for every action.** If the audit record cannot
//! be written (missing justification, database error) the action is not
//! performed and the error is returned to the caller. Non-transactional actions
//! (e.g. an in-memory kill switch) should record their intent via [`dispatch`]
//! first and apply the in-memory change only after it returns `Ok`.
//!
//! Identity: [`OperatorPrincipal`] must be built from the authenticated request
//! (the admin auth middleware's principal and the connection's source address),
//! never from a client-supplied body field.

use chrono::{DateTime, SubsecRound, Utc};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

/// `prev_hash` of the first record in the chain.
pub const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Administrative actions that must be audited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdminAction {
    KillSwitchToggle,
    CircuitBreakerOverride,
    ManualOrderIntervention,
    ConfigChange,
    ReplayTrigger,
    LimitAdjustment,
}

impl AdminAction {
    pub const ALL: [AdminAction; 6] = [
        AdminAction::KillSwitchToggle,
        AdminAction::CircuitBreakerOverride,
        AdminAction::ManualOrderIntervention,
        AdminAction::ConfigChange,
        AdminAction::ReplayTrigger,
        AdminAction::LimitAdjustment,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            AdminAction::KillSwitchToggle => "kill_switch_toggle",
            AdminAction::CircuitBreakerOverride => "circuit_breaker_override",
            AdminAction::ManualOrderIntervention => "manual_order_intervention",
            AdminAction::ConfigChange => "config_change",
            AdminAction::ReplayTrigger => "replay_trigger",
            AdminAction::LimitAdjustment => "limit_adjustment",
        }
    }

    /// High-impact actions require a non-empty justification.
    pub fn is_high_impact(&self) -> bool {
        matches!(
            self,
            AdminAction::KillSwitchToggle
                | AdminAction::CircuitBreakerOverride
                | AdminAction::ManualOrderIntervention
        )
    }
}

/// The authenticated operator performing an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorPrincipal {
    actor: String,
    source: String,
}

impl OperatorPrincipal {
    /// `actor` is the authenticated principal's identity; `source` is the
    /// client IP (as resolved by the trusted-proxy logic) or client name.
    pub fn authenticated(actor: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            actor: actor.into(),
            source: source.into(),
        }
    }

    pub fn actor(&self) -> &str {
        &self.actor
    }

    pub fn source(&self) -> &str {
        &self.source
    }
}

/// A requested administrative mutation.
#[derive(Debug, Clone)]
pub struct AdminCommand {
    pub action: AdminAction,
    /// What is acted on, e.g. `"order:<uuid>"`, `"config:swap.max_spread_bps"`.
    pub target: String,
    pub before: Option<JsonValue>,
    pub after: Option<JsonValue>,
    pub justification: Option<String>,
}

impl AdminCommand {
    pub fn validate(&self) -> Result<(), AuditError> {
        if self.target.trim().is_empty() {
            return Err(AuditError::Invalid("target is required".into()));
        }
        let justified = self
            .justification
            .as_deref()
            .is_some_and(|j| !j.trim().is_empty());
        if self.action.is_high_impact() && !justified {
            return Err(AuditError::MissingJustification(self.action));
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    #[error("justification is required for {}", .0.as_str())]
    MissingJustification(AdminAction),
    #[error("invalid admin command: {0}")]
    Invalid(String),
    #[error("audit database error: {0}")]
    Database(#[from] sqlx::Error),
}

/// A stored audit record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditRecord {
    pub seq: i64,
    pub id: Uuid,
    pub occurred_at: DateTime<Utc>,
    pub actor: String,
    pub source: String,
    pub action: String,
    pub target: String,
    pub before_state: Option<JsonValue>,
    pub after_state: Option<JsonValue>,
    pub justification: Option<String>,
    pub prev_hash: String,
    pub hash: String,
}

impl AuditRecord {
    /// SHA-256 over `prev_hash` and the record's canonical content. `seq` is
    /// excluded (assigned by the database); ordering is protected by the chain.
    pub fn compute_hash(&self) -> String {
        let canonical = serde_json::json!({
            "id": self.id,
            "occurred_at_us": self.occurred_at.timestamp_micros(),
            "actor": self.actor,
            "source": self.source,
            "action": self.action,
            "target": self.target,
            "before": self.before_state,
            "after": self.after_state,
            "justification": self.justification,
        });
        let mut h = Sha256::new();
        h.update(self.prev_hash.as_bytes());
        h.update(canonical.to_string().as_bytes());
        hex::encode(h.finalize())
    }
}

/// Verify a contiguous run of records in `seq` order. `anchor` is the
/// `prev_hash` expected for the first record (use [`GENESIS_HASH`] when
/// verifying from the start). Returns the `seq` of the first bad record.
pub fn verify_chain(records: &[AuditRecord], anchor: &str) -> Result<(), i64> {
    let mut expected_prev = anchor;
    for r in records {
        if r.prev_hash != expected_prev || r.hash != r.compute_hash() {
            return Err(r.seq);
        }
        expected_prev = &r.hash;
    }
    Ok(())
}

/// Append an audit record inside `tx`. Serialises writers with a
/// transaction-scoped advisory lock so the chain has no forks.
pub async fn record_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    principal: &OperatorPrincipal,
    cmd: &AdminCommand,
) -> Result<AuditRecord, AuditError> {
    cmd.validate()?;

    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('operator_audit_log'))")
        .execute(&mut **tx)
        .await?;
    let prev_hash: String =
        sqlx::query_scalar("SELECT hash FROM operator_audit_log ORDER BY seq DESC LIMIT 1")
            .fetch_optional(&mut **tx)
            .await?
            .unwrap_or_else(|| GENESIS_HASH.to_string());

    let mut record = AuditRecord {
        seq: 0,
        id: Uuid::new_v4(),
        // Postgres stores microseconds; truncate so the hash survives a round trip.
        occurred_at: Utc::now().trunc_subsecs(6),
        actor: principal.actor.clone(),
        source: principal.source.clone(),
        action: cmd.action.as_str().to_string(),
        target: cmd.target.clone(),
        before_state: cmd.before.clone(),
        after_state: cmd.after.clone(),
        justification: cmd.justification.clone(),
        prev_hash,
        hash: String::new(),
    };
    record.hash = record.compute_hash();

    record.seq = sqlx::query_scalar(
        "INSERT INTO operator_audit_log \
         (id, occurred_at, actor, source, action, target, before_state, after_state, \
          justification, prev_hash, hash) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) RETURNING seq",
    )
    .bind(record.id)
    .bind(record.occurred_at)
    .bind(&record.actor)
    .bind(&record.source)
    .bind(&record.action)
    .bind(&record.target)
    .bind(&record.before_state)
    .bind(&record.after_state)
    .bind(&record.justification)
    .bind(&record.prev_hash)
    .bind(&record.hash)
    .fetch_one(&mut **tx)
    .await?;

    tracing::info!(
        counter.operator_audit_records_total = 1u64,
        action = %record.action,
        actor = %record.actor,
        target = %record.target,
        "operator audit record written"
    );
    Ok(record)
}

/// The single entry point for administrative mutations: validates the command,
/// writes its audit record, runs `action` in the same transaction, and commits
/// both together. If either fails, neither is persisted.
pub async fn dispatch<T, F>(
    pool: &PgPool,
    principal: &OperatorPrincipal,
    cmd: AdminCommand,
    action: F,
) -> Result<(T, AuditRecord), AuditError>
where
    F: for<'t> FnOnce(
        &'t mut Transaction<'static, Postgres>,
    ) -> BoxFuture<'t, Result<T, AuditError>>,
{
    cmd.validate()?;
    let mut tx = pool.begin().await?;
    let record = record_in_tx(&mut tx, principal, &cmd).await?;
    let out = action(&mut tx).await?;
    tx.commit().await?;
    Ok((out, record))
}

/// Filter for [`list`].
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuditQuery {
    pub actor: Option<String>,
    pub action: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    /// Return records with `seq` greater than this (keyset pagination / export).
    pub after_seq: Option<i64>,
    pub limit: Option<i64>,
}

fn row_to_record(r: &sqlx::postgres::PgRow) -> Result<AuditRecord, sqlx::Error> {
    Ok(AuditRecord {
        seq: r.try_get("seq")?,
        id: r.try_get("id")?,
        occurred_at: r.try_get("occurred_at")?,
        actor: r.try_get("actor")?,
        source: r.try_get("source")?,
        action: r.try_get("action")?,
        target: r.try_get("target")?,
        before_state: r.try_get("before_state")?,
        after_state: r.try_get("after_state")?,
        justification: r.try_get("justification")?,
        prev_hash: r.try_get("prev_hash")?,
        hash: r.try_get("hash")?,
    })
}

/// Records matching `q`, oldest first, at most `limit` (default 100, max 1000).
pub async fn list(pool: &PgPool, q: &AuditQuery) -> Result<Vec<AuditRecord>, sqlx::Error> {
    let limit = q.limit.unwrap_or(100).clamp(1, 1000);
    let rows = sqlx::query(
        "SELECT seq, id, occurred_at, actor, source, action, target, before_state, \
                after_state, justification, prev_hash, hash \
         FROM operator_audit_log \
         WHERE ($1::text IS NULL OR actor = $1) \
           AND ($2::text IS NULL OR action = $2) \
           AND ($3::timestamptz IS NULL OR occurred_at >= $3) \
           AND ($4::timestamptz IS NULL OR occurred_at < $4) \
           AND seq > COALESCE($5, 0) \
         ORDER BY seq ASC LIMIT $6",
    )
    .bind(&q.actor)
    .bind(&q.action)
    .bind(q.from)
    .bind(q.to)
    .bind(q.after_seq)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    rows.iter().map(row_to_record).collect()
}

/// Every record from the start of the chain, with verification. Used by the
/// export endpoint for periodic internal-controls review.
pub async fn export_all(pool: &PgPool) -> Result<(Vec<AuditRecord>, Result<(), i64>), sqlx::Error> {
    let mut out = Vec::new();
    let mut after = 0;
    loop {
        let page = list(
            pool,
            &AuditQuery {
                after_seq: Some(after),
                limit: Some(1000),
                ..Default::default()
            },
        )
        .await?;
        let Some(last) = page.last() else { break };
        after = last.seq;
        out.extend(page);
    }
    let verified = verify_chain(&out, GENESIS_HASH);
    Ok((out, verified))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(n: usize) -> Vec<AuditRecord> {
        let mut prev = GENESIS_HASH.to_string();
        (0..n)
            .map(|i| {
                let mut r = AuditRecord {
                    seq: i as i64 + 1,
                    id: Uuid::new_v4(),
                    occurred_at: Utc::now().trunc_subsecs(6),
                    actor: "ops@example".into(),
                    source: "10.0.0.1".into(),
                    action: AdminAction::ConfigChange.as_str().into(),
                    target: format!("config:k{i}"),
                    before_state: Some(serde_json::json!({ "v": i })),
                    after_state: Some(serde_json::json!({ "v": i + 1 })),
                    justification: None,
                    prev_hash: prev.clone(),
                    hash: String::new(),
                };
                r.hash = r.compute_hash();
                prev = r.hash.clone();
                r
            })
            .collect()
    }

    #[test]
    fn intact_chain_verifies() {
        assert_eq!(verify_chain(&chain(5), GENESIS_HASH), Ok(()));
    }

    #[test]
    fn edited_record_is_detected() {
        let mut c = chain(5);
        c[2].after_state = Some(serde_json::json!({ "v": 999 }));
        assert_eq!(verify_chain(&c, GENESIS_HASH), Err(3));
    }

    #[test]
    fn deleted_record_is_detected() {
        let mut c = chain(5);
        c.remove(1);
        assert_eq!(verify_chain(&c, GENESIS_HASH), Err(3));
    }

    #[test]
    fn rehashed_edit_is_detected_by_successor() {
        let mut c = chain(3);
        c[0].actor = "someone-else".into();
        c[0].hash = c[0].compute_hash();
        assert_eq!(verify_chain(&c, GENESIS_HASH), Err(2));
    }

    #[test]
    fn high_impact_actions_require_justification() {
        for action in AdminAction::ALL {
            let mut cmd = AdminCommand {
                action,
                target: "t".into(),
                before: None,
                after: None,
                justification: Some("   ".into()),
            };
            assert_eq!(
                cmd.validate().is_err(),
                action.is_high_impact(),
                "{action:?}"
            );
            cmd.justification = Some("incident INC-42".into());
            assert!(cmd.validate().is_ok());
        }
    }

    #[test]
    fn empty_target_is_rejected() {
        let cmd = AdminCommand {
            action: AdminAction::ReplayTrigger,
            target: " ".into(),
            before: None,
            after: None,
            justification: None,
        };
        assert!(matches!(cmd.validate(), Err(AuditError::Invalid(_))));
    }

    #[test]
    fn action_names_are_unique_and_match_serde() {
        let names: std::collections::HashSet<_> =
            AdminAction::ALL.iter().map(|a| a.as_str()).collect();
        assert_eq!(names.len(), AdminAction::ALL.len());
        for a in AdminAction::ALL {
            assert_eq!(serde_json::to_value(a).unwrap(), a.as_str());
        }
    }
}
