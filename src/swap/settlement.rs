//! Settlement of executed swap hand-offs.
//!
//! Takes a settled [`SwapHandoff`] and drives it to an outbound transfer or
//! credited balance through a [`SettlementBackend`] (the Stellar account credit /
//! anchor submission seam). Progress is persisted in `swap_settlements`:
//!
//! ```text
//! pending -> settlement_pending -> settled
//!                               -> failed
//! ```
//!
//! - `settlement_pending` is written **before** the backend is called, so a
//!   crash mid-submit leaves a visible row for [`SwapSettler::reconcile`].
//! - Backends must be idempotent on `idempotency_key`; resubmitting after an
//!   ambiguous outcome is therefore safe.
//! - `settled` is reached only after the backend confirms, and is terminal.
//!   `failed` is terminal and carries `last_error`.
//!
//! Cross-chain bridge settlement is out of scope and will sit behind the same
//! backend seam in a later phase.

use async_trait::async_trait;
use sqlx::PgPool;

use crate::db::models::SwapSettlement;
use crate::db::queries;
use crate::swap::SwapHandoff;

pub const STATUS_PENDING: &str = "pending";
pub const STATUS_SETTLEMENT_PENDING: &str = "settlement_pending";
pub const STATUS_SETTLED: &str = "settled";
pub const STATUS_FAILED: &str = "failed";

const RECONCILE_BATCH_SIZE: i64 = 100;

/// Failure reported by a [`SettlementBackend`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SettlementBackendError {
    /// The settlement target definitively refused the transfer. Terminal.
    #[error("settlement rejected: {0}")]
    Rejected(String),
    /// Outcome unknown (timeout, outage, dropped connection). Left for reconciliation.
    #[error("settlement outcome unknown: {0}")]
    Unavailable(String),
}

/// What a [`SettlementBackend`] reports for a given idempotency key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettlementLookup {
    /// The backend has no record of a settlement with this key.
    NotFound,
    /// Submitted, awaiting confirmation on the settlement target.
    Pending,
    /// Confirmed on the settlement target.
    Confirmed { settlement_ref: String },
    /// The settlement target reports the settlement failed.
    Failed(String),
}

/// The interface a settlement target must satisfy (Stellar account credit,
/// anchor submission, ...). Implementations must be idempotent on
/// `settlement.idempotency_key`.
#[async_trait]
pub trait SettlementBackend: Send + Sync {
    /// Short identifier for logs/metrics (e.g. `"horizon"`, `"anchor"`).
    fn name(&self) -> &str;

    /// Submit the outbound transfer / credit. Returns the settlement reference
    /// (e.g. a Horizon transaction hash).
    async fn submit(&self, settlement: &SwapSettlement) -> Result<String, SettlementBackendError>;

    /// Ask the settlement target what it knows about `idempotency_key`.
    async fn lookup(&self, idempotency_key: &str) -> Result<SettlementLookup, SettlementBackendError>;
}

/// Drives swap hand-offs through a [`SettlementBackend`] and persists progress.
pub struct SwapSettler<B: SettlementBackend> {
    pool: PgPool,
    backend: B,
}

impl<B: SettlementBackend> SwapSettler<B> {
    pub fn new(pool: PgPool, backend: B) -> Self {
        Self { pool, backend }
    }

    /// Settle one executed hand-off. Idempotent: repeating the call for the same
    /// hand-off resumes from the stored state instead of submitting twice.
    pub async fn settle(&self, handoff: &SwapHandoff) -> sqlx::Result<SwapSettlement> {
        let row = queries::upsert_swap_settlement(
            &self.pool,
            &handoff.idempotency_key,
            handoff.transaction_id,
            handoff.tenant_id,
            &handoff.stellar_account,
            &handoff.source_asset,
            &handoff.amount,
        )
        .await?;
        self.advance(row).await
    }

    /// Advance every non-terminal settlement. Run on startup and periodically;
    /// this is how interrupted and ambiguous settlements resolve. A row that
    /// fails to advance is logged and stays open, so the next run retries it.
    pub async fn reconcile(&self) -> sqlx::Result<Vec<SwapSettlement>> {
        let open = queries::list_open_swap_settlements(&self.pool, RECONCILE_BATCH_SIZE).await?;
        let mut advanced = Vec::with_capacity(open.len());
        for row in open {
            let key = row.idempotency_key.clone();
            match self.advance(row).await {
                Ok(next) => advanced.push(next),
                Err(e) => tracing::error!(
                    idempotency_key = %key,
                    error = %e,
                    "swap settlement reconciliation failed; will retry"
                ),
            }
        }
        Ok(advanced)
    }

    /// Move one settlement toward a terminal state. No-op on terminal rows.
    async fn advance(&self, mut row: SwapSettlement) -> sqlx::Result<SwapSettlement> {
        if row.status == STATUS_SETTLED || row.status == STATUS_FAILED {
            return Ok(row);
        }

        if row.status == STATUS_PENDING {
            // Persist intent before touching the backend. If another worker got
            // here first, it owns the row now.
            match queries::transition_swap_settlement(
                &self.pool,
                &row.idempotency_key,
                STATUS_PENDING,
                STATUS_SETTLEMENT_PENDING,
                None,
                None,
            )
            .await?
            {
                Some(next) => row = next,
                None => return self.reload(&row.idempotency_key).await,
            }
        }

        // The backend may already hold this settlement from a run that crashed
        // after submitting, so check before submitting again.
        match self.backend.lookup(&row.idempotency_key).await {
            Ok(SettlementLookup::Confirmed { settlement_ref }) => {
                return self.apply(&row, STATUS_SETTLED, Some(&settlement_ref), None).await
            }
            Ok(SettlementLookup::Failed(reason)) => {
                return self.apply(&row, STATUS_FAILED, None, Some(&reason)).await
            }
            Ok(SettlementLookup::Pending) => return Ok(row),
            Ok(SettlementLookup::NotFound) => {}
            Err(e) => return self.record_error(&row, &e.to_string()).await,
        }

        match self.backend.submit(&row).await {
            Ok(settlement_ref) => {
                // Record the reference before confirming, so a crash here still
                // leaves something to reconcile against.
                row = self
                    .apply(&row, STATUS_SETTLEMENT_PENDING, Some(&settlement_ref), None)
                    .await?;
                match self.backend.lookup(&row.idempotency_key).await {
                    Ok(SettlementLookup::Confirmed { settlement_ref }) => {
                        self.apply(&row, STATUS_SETTLED, Some(&settlement_ref), None).await
                    }
                    Ok(SettlementLookup::Failed(reason)) => {
                        self.apply(&row, STATUS_FAILED, None, Some(&reason)).await
                    }
                    Ok(_) => Ok(row),
                    Err(e) => self.record_error(&row, &e.to_string()).await,
                }
            }
            Err(SettlementBackendError::Rejected(reason)) => {
                self.apply(&row, STATUS_FAILED, None, Some(&reason)).await
            }
            Err(e @ SettlementBackendError::Unavailable(_)) => {
                self.record_error(&row, &e.to_string()).await
            }
        }
    }

    /// Compare-and-set `row` from its current status to `to`. If another worker
    /// moved the row first, return its current state instead.
    async fn apply(
        &self,
        row: &SwapSettlement,
        to: &str,
        settlement_ref: Option<&str>,
        last_error: Option<&str>,
    ) -> sqlx::Result<SwapSettlement> {
        match queries::transition_swap_settlement(
            &self.pool,
            &row.idempotency_key,
            &row.status,
            to,
            settlement_ref,
            last_error,
        )
        .await?
        {
            Some(next) => Ok(next),
            None => self.reload(&row.idempotency_key).await,
        }
    }

    /// Keep the row non-terminal and record why, so the failure stays visible.
    async fn record_error(&self, row: &SwapSettlement, reason: &str) -> sqlx::Result<SwapSettlement> {
        tracing::warn!(
            idempotency_key = %row.idempotency_key,
            backend = self.backend.name(),
            reason,
            "swap settlement outcome unknown; will reconcile"
        );
        self.apply(row, &row.status, None, Some(reason)).await
    }

    async fn reload(&self, idempotency_key: &str) -> sqlx::Result<SwapSettlement> {
        queries::get_swap_settlement(&self.pool, idempotency_key)
            .await?
            .ok_or(sqlx::Error::RowNotFound)
    }
}
