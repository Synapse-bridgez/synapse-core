//! Settlement: delivering the swap output once an order has executed.
//!
//! Defines the [`SettlementAdapter`] boundary. Adapters for specific rails
//! (Stellar payments, cross-chain bridges, ...) are implemented in later
//! Phase 2 issues.

use async_trait::async_trait;

use super::error::SwapError;
use super::execution::ExecutionReport;
use super::order::SwapOrder;

/// Proof that a settlement was submitted, returned by a [`SettlementAdapter`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettlementReceipt {
    /// Rail-specific reference (transaction hash, bridge message id, ...).
    pub reference: String,
}

/// Delivers executed swap output over a specific settlement rail.
#[async_trait]
pub trait SettlementAdapter: Send + Sync {
    /// Settle the output of `execution` for `order`. Implementations must be
    /// idempotent on `order.idempotency_key`.
    async fn settle(
        &self,
        order: &SwapOrder,
        execution: &ExecutionReport,
    ) -> Result<SettlementReceipt, SwapError>;
}
