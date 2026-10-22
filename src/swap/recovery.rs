//! Stuck-transaction watchdog, dead-letter quarantine, and manual intervention
//! tooling (#1446).
//!
//! A transaction still in `processing` past the configured threshold has most
//! likely lost its worker. The watchdog moves it to `dlq` and writes a
//! `transaction_dlq` row. Operators then resolve it through [`intervene`], which
//! uses the same `TRANSACTION_TRANSITIONS` table as the normal processor and
//! writes an audit row for every call, applied or not.
//!
//! Race safety: the watchdog's UPDATE re-checks `status = 'processing'` and
//! staleness, and the processor's completion UPDATE is guarded on
//! `status = 'processing'`. Whichever runs second matches no rows.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use sqlx::types::BigDecimal;
use sqlx::PgPool;
use tracing::{error, warn};
use uuid::Uuid;

use crate::config::Config;
use crate::db::audit::{AuditLog, ENTITY_TRANSACTION};
use crate::db::queries;
use crate::error::AppError;
use crate::validation::state_transitions::{is_valid_transition, TRANSACTION_TRANSITIONS};

/// Audit actor for automatic watchdog actions.
pub const WATCHDOG_ACTOR: &str = "watchdog";

/// Upper bound on rows quarantined per tick, so a single tick stays short.
const QUARANTINE_BATCH: i64 = 100;

/// Operator resolutions for a quarantined transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intervention {
    /// Requeue to `pending` so the normal processor picks it up again.
    Retry,
    /// Mark `completed`. The operator asserts the work is done.
    ForceComplete,
    /// Mark `failed`. Refused if a Horizon payment was already submitted.
    ForceCancel,
}

impl Intervention {
    pub fn action(self) -> &'static str {
        match self {
            Intervention::Retry => "dlq_retry",
            Intervention::ForceComplete => "dlq_force_complete",
            Intervention::ForceCancel => "dlq_force_cancel",
        }
    }

    pub fn target_status(self) -> &'static str {
        match self {
            Intervention::Retry => "pending",
            Intervention::ForceComplete => "completed",
            Intervention::ForceCancel => "failed",
        }
    }
}

/// Result of an intervention. `applied: false` means the transaction was already
/// in the target status, so the call changed nothing.
#[derive(Debug, Clone, Serialize)]
pub struct InterventionOutcome {
    pub transaction_id: Uuid,
    pub action: &'static str,
    pub previous_status: String,
    pub status: String,
    pub applied: bool,
}

/// Start the watchdog loop. Mirrors `PartitionManager::start`: one spawned task
/// on a fixed interval. Each tick quarantines stale rows, then checks DLQ depth
/// and age against the alert thresholds.
pub fn spawn_watchdog(pool: PgPool, config: &Config) {
    let stale_after = Duration::from_secs(config.stuck_processing_threshold_secs);
    let interval = Duration::from_secs(config.watchdog_interval_secs);
    let depth_threshold = config.dlq_depth_alert_threshold;
    let age_threshold = Duration::from_secs(config.dlq_age_alert_secs);

    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            match quarantine_stale_processing(&pool, stale_after).await {
                Ok(0) => {}
                Ok(moved) => {
                    warn!(moved, "Stuck-transaction watchdog moved transactions to DLQ")
                }
                Err(e) => error!(error = %e, "Stuck-transaction watchdog scan failed"),
            }
            if let Err(e) = check_dlq_alerts(&pool, depth_threshold, age_threshold).await {
                error!(error = %e, "DLQ alert check failed");
            }
        }
    });
}

/// Move up to `QUARANTINE_BATCH` stale `processing` transactions to `dlq`.
/// Returns how many were moved.
pub async fn quarantine_stale_processing(
    pool: &PgPool,
    stale_after: Duration,
) -> Result<usize, sqlx::Error> {
    let ids =
        queries::find_stale_processing_ids(pool, stale_after.as_secs_f64(), QUARANTINE_BATCH)
            .await?;

    let mut moved = 0;
    for id in ids {
        if quarantine_one(pool, id, stale_after).await? {
            moved += 1;
        }
    }
    Ok(moved)
}

async fn quarantine_one(
    pool: &PgPool,
    id: Uuid,
    stale_after: Duration,
) -> Result<bool, sqlx::Error> {
    let mut tx = pool.begin().await?;

    // Re-check status and staleness under the row lock: the processor may have
    // completed the row between the scan and this update. Dropping `tx` on the
    // early return rolls back.
    let row: Option<(Uuid, String, BigDecimal, String, Option<String>, DateTime<Utc>)> =
        sqlx::query_as(
            "UPDATE transactions SET status = 'dlq', updated_at = NOW() \
             WHERE id = $1 AND status = 'processing' \
               AND updated_at < NOW() - make_interval(secs => $2) \
             RETURNING id, stellar_account, amount, asset_code, anchor_transaction_id, created_at",
        )
        .bind(id)
        .bind(stale_after.as_secs_f64())
        .fetch_optional(&mut *tx)
        .await?;

    let Some((tx_id, account, amount, asset_code, anchor_id, created_at)) = row else {
        return Ok(false);
    };

    sqlx::query(
        "INSERT INTO transaction_dlq \
         (id, transaction_id, stellar_account, amount, asset_code, anchor_transaction_id, \
          error_reason, retry_count, original_created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 0, $8)",
    )
    .bind(Uuid::new_v4())
    .bind(tx_id)
    .bind(account)
    .bind(amount)
    .bind(asset_code)
    .bind(anchor_id)
    .bind("stuck in processing past threshold; no worker progress observed")
    .bind(created_at)
    .execute(&mut *tx)
    .await?;

    AuditLog::log(
        &mut tx,
        tx_id,
        ENTITY_TRANSACTION,
        "dlq_quarantine",
        Some(json!({ "status": "processing" })),
        Some(json!({ "status": "dlq", "stale_after_secs": stale_after.as_secs() })),
        WATCHDOG_ACTOR,
    )
    .await?;

    tx.commit().await?;
    Ok(true)
}

/// Apply an operator intervention to a transaction. Idempotent: if the
/// transaction is already in the target status the call is a no-op. Every call,
/// applied or not, writes an audit row carrying the actor and reason.
pub async fn intervene(
    pool: &PgPool,
    transaction_id: Uuid,
    op: Intervention,
    actor: &str,
    reason: &str,
) -> Result<InterventionOutcome, AppError> {
    let mut tx = pool.begin().await?;

    let (current, horizon_payment_id): (String, Option<String>) = sqlx::query_as(
        "SELECT status, horizon_payment_id FROM transactions WHERE id = $1 FOR UPDATE",
    )
    .bind(transaction_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("transaction {transaction_id}")))?;

    let target = op.target_status();
    let applied = current != target;

    if applied {
        if !is_valid_transition(&current, target, TRANSACTION_TRANSITIONS) {
            return Err(AppError::InvalidStatusTransition(format!(
                "{} cannot move '{current}' to '{target}'",
                op.action()
            )));
        }
        // No compensation path exists yet, so cancelling after a payment has been
        // submitted would silently drop funds. Refuse and leave it to a human.
        if op == Intervention::ForceCancel && horizon_payment_id.is_some() {
            return Err(AppError::ConcurrentModification(
                "a Horizon payment was already submitted; cancelling would drop funds without compensation"
                    .to_string(),
            ));
        }

        sqlx::query(
            "UPDATE transactions SET status = $2, updated_at = NOW() WHERE id = $1 AND status = $3",
        )
        .bind(transaction_id)
        .bind(target)
        .bind(&current)
        .execute(&mut *tx)
        .await?;

        if op == Intervention::Retry {
            sqlx::query(
                "UPDATE transaction_dlq SET retry_count = retry_count + 1, last_retry_at = NOW() \
                 WHERE transaction_id = $1",
            )
            .bind(transaction_id)
            .execute(&mut *tx)
            .await?;
        }
    }

    AuditLog::log(
        &mut tx,
        transaction_id,
        ENTITY_TRANSACTION,
        op.action(),
        Some(json!({ "status": current })),
        Some(json!({ "status": target, "applied": applied, "reason": reason })),
        actor,
    )
    .await?;

    tx.commit().await?;

    Ok(InterventionOutcome {
        transaction_id,
        action: op.action(),
        previous_status: current,
        status: target.to_string(),
        applied,
    })
}

/// Log at error level when unresolved DLQ depth or oldest age crosses its
/// threshold. The Loki rule in `alerting/loki-rules.yml` matches this message.
async fn check_dlq_alerts(
    pool: &PgPool,
    depth_threshold: i64,
    age_threshold: Duration,
) -> Result<(), sqlx::Error> {
    let (depth, oldest) = queries::dlq_depth_and_oldest(pool).await?;
    let Some(oldest) = oldest else {
        return Ok(());
    };

    let oldest_age_secs = (Utc::now() - oldest).num_seconds();
    if depth >= depth_threshold || oldest_age_secs >= age_threshold.as_secs() as i64 {
        error!(
            dlq_depth = depth,
            oldest_age_secs,
            depth_threshold,
            age_threshold_secs = age_threshold.as_secs(),
            "Transaction DLQ threshold crossed"
        );
    }
    Ok(())
}
