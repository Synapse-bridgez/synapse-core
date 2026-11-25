//! Database queries used exclusively by the fiat→swap backfill job (#1475).
//!
//! All writes use `ON CONFLICT DO NOTHING` so re-running the job against
//! already-processed rows is safe.  Callers that need idempotency at the
//! swap-order level should additionally check `swap_backfill_state` before
//! inserting into `swap_quotes` / `swap_orders`.

use chrono::{DateTime, Utc};
use sqlx::types::BigDecimal;
use sqlx::{PgExecutor, Result};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Source: Phase 1 transactions
// ---------------------------------------------------------------------------

/// A Phase 1 transaction row as read during backfill.  Only the columns
/// required to construct a swap hand-off are selected; the full `Transaction`
/// model is not used to avoid pulling in async-graphql derives.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct BackfillSourceRow {
    pub id: Uuid,
    pub stellar_account: String,
    pub amount: BigDecimal,
    pub asset_code: String,
    pub callback_status: Option<String>,
    pub anchor_transaction_id: Option<String>,
    pub memo: Option<String>,
    pub memo_type: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Fetch a batch of eligible Phase 1 transactions that have not yet been
/// backfilled.  "Eligible" means `status = 'completed'` and the row's id is
/// not already present in `swap_backfill_state` (regardless of dry-run flag).
///
/// Rows are returned in ascending `id` order so the caller can use the last
/// seen id as a cursor for the next batch, enabling restart-from-checkpoint.
pub async fn fetch_eligible_batch<'e>(
    executor: impl PgExecutor<'e>,
    after_id: Option<Uuid>,
    batch_size: i64,
) -> Result<Vec<BackfillSourceRow>> {
    // Use a correlated NOT EXISTS rather than a LEFT JOIN / IS NULL so the
    // query planner can short-circuit on the first matching backfill row.
    if let Some(cursor) = after_id {
        sqlx::query_as::<_, BackfillSourceRow>(
            "SELECT t.id, t.stellar_account, t.amount, t.asset_code, \
                    t.callback_status, t.anchor_transaction_id, \
                    t.memo, t.memo_type, t.metadata, t.created_at, t.updated_at \
             FROM transactions t \
             WHERE t.status = 'completed' \
               AND t.id > $1 \
               AND NOT EXISTS ( \
                   SELECT 1 FROM swap_backfill_state s \
                   WHERE s.transaction_id = t.id \
               ) \
             ORDER BY t.id \
             LIMIT $2",
        )
        .bind(cursor)
        .bind(batch_size)
        .fetch_all(executor)
        .await
    } else {
        sqlx::query_as::<_, BackfillSourceRow>(
            "SELECT t.id, t.stellar_account, t.amount, t.asset_code, \
                    t.callback_status, t.anchor_transaction_id, \
                    t.memo, t.memo_type, t.metadata, t.created_at, t.updated_at \
             FROM transactions t \
             WHERE t.status = 'completed' \
               AND NOT EXISTS ( \
                   SELECT 1 FROM swap_backfill_state s \
                   WHERE s.transaction_id = t.id \
               ) \
             ORDER BY t.id \
             LIMIT $1",
        )
        .bind(batch_size)
        .fetch_all(executor)
        .await
    }
}

/// Count of eligible (not yet processed) completed transactions.
/// Used by --dry-run to report expected work before any write.
pub async fn count_eligible<'e>(executor: impl PgExecutor<'e>) -> Result<i64> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM transactions t \
         WHERE t.status = 'completed' \
           AND NOT EXISTS ( \
               SELECT 1 FROM swap_backfill_state s \
               WHERE s.transaction_id = t.id \
           )",
    )
    .fetch_one(executor)
    .await
}

// ---------------------------------------------------------------------------
// Checkpoint: swap_backfill_state
// ---------------------------------------------------------------------------

/// Record that `transaction_id` was processed in this run.  The primary-key
/// conflict guard makes this safe to call on re-runs.
pub async fn record_backfill_state<'e>(
    executor: impl PgExecutor<'e>,
    transaction_id: Uuid,
    swap_order_id: Option<Uuid>,
    dry_run: bool,
    run_id: &str,
    outcome: &str,
    error_detail: Option<&str>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO swap_backfill_state \
           (transaction_id, swap_order_id, dry_run, backfill_run_id, outcome, error_detail) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (transaction_id) DO NOTHING",
    )
    .bind(transaction_id)
    .bind(swap_order_id)
    .bind(dry_run)
    .bind(run_id)
    .bind(outcome)
    .bind(error_detail)
    .execute(executor)
    .await
    .map(|_| ())
}

/// Progress summary for the current run — counts by outcome.
pub async fn backfill_progress<'e>(
    executor: impl PgExecutor<'e>,
    run_id: &str,
) -> Result<BackfillProgress> {
    sqlx::query_as::<_, BackfillProgress>(
        "SELECT \
           COALESCE(SUM(CASE WHEN outcome = 'created' THEN 1 ELSE 0 END), 0) AS created, \
           COALESCE(SUM(CASE WHEN outcome = 'skipped' THEN 1 ELSE 0 END), 0) AS skipped, \
           COALESCE(SUM(CASE WHEN outcome = 'error'   THEN 1 ELSE 0 END), 0) AS errors \
         FROM swap_backfill_state \
         WHERE backfill_run_id = $1",
    )
    .bind(run_id)
    .fetch_one(executor)
    .await
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct BackfillProgress {
    pub created: i64,
    pub skipped: i64,
    pub errors: i64,
}
