//! Execution: routing a [`SwapOrder`] to a venue and reporting the fill.
//!
//! Defines the [`ExecutionVenue`] boundary. A venue may be an on-chain DEX, an
//! off-chain market maker, or an internal book; none are implemented here.

use async_trait::async_trait;
use sqlx::types::BigDecimal;

use super::error::SwapError;
use super::order::SwapOrder;

/// Result of executing an order on a venue.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionReport {
    /// Venue-side identifier for the execution, for reconciliation.
    pub venue_order_id: String,
    /// Amount actually filled, in the source asset's units.
    pub filled_amount: BigDecimal,
}

/// A venue that executes swap orders.
#[async_trait]
pub trait ExecutionVenue: Send + Sync {
    /// Execute `order`. Implementations must be idempotent on
    /// `order.idempotency_key`.
    async fn execute(&self, order: &SwapOrder) -> Result<ExecutionReport, SwapError>;
}
