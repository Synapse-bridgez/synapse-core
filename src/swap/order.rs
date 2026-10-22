//! Swap orders: the engine's record of one accepted hand-off being converted.
//!
//! An order is created from a [`SwapHandoff`](super::SwapHandoff) and,
//! optionally, a [`Quote`](super::quote::Quote). It is keyed by the hand-off's
//! idempotency key so redelivered hand-offs map to the same order. No order
//! persistence or state-transition logic lives here yet.

use sqlx::types::BigDecimal;
use uuid::Uuid;

/// Lifecycle state of a [`SwapOrder`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderStatus {
    /// Accepted, not yet risk-checked or executed.
    Pending,
    /// Handed to an execution venue.
    Executing,
    /// Executed and settled.
    Settled,
    /// Terminally failed; see the emitted failure event for the reason.
    Failed,
}

/// A swap order created from an accepted hand-off.
#[derive(Debug, Clone, PartialEq)]
pub struct SwapOrder {
    /// Idempotency key of the originating hand-off (`SwapHandoff::idempotency_key`).
    pub idempotency_key: String,
    /// The Phase 1 transaction this order swaps.
    pub transaction_id: Uuid,
    /// Quote the order was placed against, if one was issued.
    pub quote_id: Option<String>,
    /// Amount to swap, in the source asset's units.
    pub amount: BigDecimal,
    pub status: OrderStatus,
}
