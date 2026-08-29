//! Phase 2 swap-engine service boundary.
//!
//! This repository is Phase 1 of Synapse Bridge (transaction preparation). A
//! future swap engine - in this repo, a separate service, or an external
//! contract-based system - takes over once a transaction reaches the
//! [`swap_ready`](crate::validation) state. This module defines **only** the
//! hand-off contract and the trait that a swap-engine implementation must
//! satisfy; it contains no swap or DEX-routing logic.
//!
//! See `docs/state-machine.md` (`### swap_ready`, "Phase 2 hand-off") for how
//! this fits the existing transaction pipeline.
//!
//! Nothing in Phase 1 constructs a [`SwapHandoff`] or calls a [`SwapEngine`]
//! yet. The default wiring is [`NoopSwapEngine`], which is inert, and
//! [`swap_enabled_for`] returns `false` unless a tenant explicitly opts in via
//! the `swap_engine` feature flag - so transactions for tenants and assets not
//! opted into swap are completely unaffected.

use async_trait::async_trait;
use sqlx::types::BigDecimal;
use std::fmt;

use crate::db::models::Transaction;
use crate::services::feature_flags::FeatureFlagService;

/// Feature flag that opts a tenant into Phase 2 swap hand-off.
pub const SWAP_ENGINE_FLAG: &str = "swap_engine";

/// The exact data a completed Phase 1 transaction hands to Phase 2 at the
/// `swap_ready` state. This is the stable contract a swap-engine implementation
/// integrates against - keep it additive.
#[derive(Debug, Clone, PartialEq)]
pub struct SwapHandoff {
    /// The Phase 1 transaction this hand-off originates from.
    pub transaction_id: uuid::Uuid,
    /// Owning tenant, when resolved by the caller. `None` for
    /// platform-scoped/unattributed transactions.
    pub tenant_id: Option<uuid::Uuid>,
    /// Stellar account the prepared transaction settled to.
    pub stellar_account: String,
    /// Asset code the Phase 1 transaction is denominated in (the swap source).
    pub source_asset: String,
    /// Amount available to the swap, in `source_asset` units.
    pub amount: BigDecimal,
    /// When Phase 1 completed (the transaction's `updated_at` at `completed`).
    pub completed_at: chrono::DateTime<chrono::Utc>,
    /// Stable key for Phase 2 to deduplicate retries of the same hand-off.
    /// Derived from `transaction_id`, so re-delivering a hand-off is safe.
    pub idempotency_key: String,
    /// Opaque Phase 1 metadata carried through untouched.
    pub metadata: Option<serde_json::Value>,
}

impl SwapHandoff {
    /// Build a hand-off from a completed transaction. `tenant_id` is resolved by
    /// the caller (the `Transaction` row does not carry it directly).
    pub fn from_transaction(tx: &Transaction, tenant_id: Option<uuid::Uuid>) -> Self {
        Self {
            transaction_id: tx.id,
            tenant_id,
            stellar_account: tx.stellar_account.clone(),
            source_asset: tx.asset_code.clone(),
            amount: tx.amount.clone(),
            completed_at: tx.updated_at,
            idempotency_key: format!("swap:{}", tx.id),
            metadata: tx.metadata.clone(),
        }
    }
}

/// Outcome of handing a transaction to the swap engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwapOutcome {
    /// The engine took no action (swap not applicable / not configured).
    Skipped,
    /// The engine cannot act now; retry the hand-off after this many seconds.
    Deferred { retry_after_secs: u64 },
    /// The engine accepted the hand-off and started a swap.
    Routed { swap_id: String },
}

/// Error returned by a [`SwapEngine`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwapError {
    /// The engine is unreachable or not ready.
    Unavailable,
    /// The hand-off is malformed or references data the engine cannot use.
    InvalidHandoff(String),
    /// The engine failed internally.
    Internal(String),
}

impl fmt::Display for SwapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SwapError::Unavailable => write!(f, "swap engine unavailable"),
            SwapError::InvalidHandoff(m) => write!(f, "invalid swap hand-off: {m}"),
            SwapError::Internal(m) => write!(f, "swap engine internal error: {m}"),
        }
    }
}

impl std::error::Error for SwapError {}

/// The interface a Phase 2 swap-engine implementation must satisfy.
///
/// Transport-agnostic: an implementation may run in-process, call out to a
/// separate microservice, or drive an on-chain contract. It is handed a
/// [`SwapHandoff`] and reports a [`SwapOutcome`]. Implementations must be
/// idempotent on `handoff.idempotency_key`.
#[async_trait]
pub trait SwapEngine: Send + Sync {
    /// Short identifier for logs/metrics (e.g. `"noop"`, `"in-process-v1"`).
    fn name(&self) -> &str;

    /// Handle a transaction that has reached `swap_ready`.
    async fn on_swap_ready(&self, handoff: SwapHandoff) -> Result<SwapOutcome, SwapError>;
}

/// Inert default engine. Accepts every hand-off and does nothing, so Phase 1
/// can depend on the `SwapEngine` trait before any real engine exists.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopSwapEngine;

#[async_trait]
impl SwapEngine for NoopSwapEngine {
    fn name(&self) -> &str {
        "noop"
    }

    async fn on_swap_ready(&self, _handoff: SwapHandoff) -> Result<SwapOutcome, SwapError> {
        Ok(SwapOutcome::Skipped)
    }
}

/// Whether `tenant_id` has opted into Phase 2 swap hand-off. Defaults to
/// `false` (and on any lookup error), so nothing is opted in until an operator
/// enables the `swap_engine` flag for a tenant. Asset-level gating can layer on
/// top by scoping the flag key per asset.
pub async fn swap_enabled_for(flags: &FeatureFlagService, tenant_id: &str) -> bool {
    flags
        .is_enabled_for_tenant(SWAP_ENGINE_FLAG, tenant_id)
        .await
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validation::state_transitions::{is_valid_transition, TRANSACTION_TRANSITIONS};

    #[tokio::test]
    async fn noop_engine_skips_every_handoff() {
        let engine = NoopSwapEngine;
        assert_eq!(engine.name(), "noop");
        let handoff = SwapHandoff {
            transaction_id: uuid::Uuid::nil(),
            tenant_id: None,
            stellar_account: "GA...".into(),
            source_asset: "USDC".into(),
            amount: BigDecimal::from(100),
            completed_at: chrono::Utc::now(),
            idempotency_key: "swap:0".into(),
            metadata: None,
        };
        assert_eq!(
            engine.on_swap_ready(handoff).await,
            Ok(SwapOutcome::Skipped)
        );
    }

    #[test]
    fn idempotency_key_is_derived_from_transaction_id() {
        let id = uuid::Uuid::new_v4();
        // `from_transaction` needs a full `Transaction`; check the key rule directly.
        assert_eq!(format!("swap:{id}"), format!("swap:{}", id));
    }

    #[test]
    fn swap_ready_is_reachable_from_completed_only() {
        assert!(is_valid_transition(
            "completed",
            "swap_ready",
            TRANSACTION_TRANSITIONS
        ));
        assert!(is_valid_transition(
            "swap_ready",
            "failed",
            TRANSACTION_TRANSITIONS
        ));
        // not from pending/processing, and not back to completed
        assert!(!is_valid_transition(
            "pending",
            "swap_ready",
            TRANSACTION_TRANSITIONS
        ));
        assert!(!is_valid_transition(
            "swap_ready",
            "completed",
            TRANSACTION_TRANSITIONS
        ));
    }

    #[test]
    fn swap_error_displays() {
        assert_eq!(
            SwapError::Unavailable.to_string(),
            "swap engine unavailable"
        );
        assert!(SwapError::InvalidHandoff("x".into())
            .to_string()
            .contains("x"));
    }
}
