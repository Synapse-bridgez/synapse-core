//! Domain events emitted by the swap engine as hand-offs move through quoting,
//! execution and settlement.
//!
//! Only the event shapes are defined here. Emission and delivery (logs,
//! metrics, webhooks) are wired in later Phase 2 issues.

use uuid::Uuid;

/// A lifecycle event for a swap hand-off or order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwapEvent {
    /// The engine accepted a hand-off from Phase 1.
    HandoffAccepted { transaction_id: Uuid },
    /// A quote was issued for a hand-off.
    QuoteIssued { quote_id: String },
    /// An order settled successfully.
    OrderSettled { idempotency_key: String },
    /// An order terminally failed.
    OrderFailed {
        idempotency_key: String,
        reason: String,
    },
}
