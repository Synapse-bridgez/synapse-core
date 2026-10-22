//! Swap order domain model and status transition guard.
//!
//! The transition table here is the single source of truth for swap order
//! status; persistence goes through [`crate::swap::execution::transition_order`].

use crate::validation::state_transitions::{is_valid_transition, Transition};
use sqlx::types::BigDecimal;
use uuid::Uuid;

pub const SWAP_ORDER_TRANSITIONS: &[Transition] = &[
    Transition {
        from: "quoted",
        to: "executing",
    },
    Transition {
        from: "quoted",
        to: "failed",
    },
    Transition {
        from: "executing",
        to: "partially_filled",
    },
    Transition {
        from: "executing",
        to: "filled",
    },
    Transition {
        from: "executing",
        to: "failed",
    },
    Transition {
        from: "executing",
        to: "reconciling",
    },
    Transition {
        from: "partially_filled",
        to: "filled",
    },
    Transition {
        from: "partially_filled",
        to: "failed",
    },
    Transition {
        from: "partially_filled",
        to: "reconciling",
    },
    Transition {
        from: "reconciling",
        to: "partially_filled",
    },
    Transition {
        from: "reconciling",
        to: "filled",
    },
    Transition {
        from: "reconciling",
        to: "failed",
    },
];

/// Statuses a worker may lease for execution.
pub const EXECUTABLE_STATUSES: &[&str] =
    &["quoted", "executing", "partially_filled", "reconciling"];

/// Machine-readable failure reason codes stored in `swap_orders.failure_code`.
pub const FAILURE_RETRIES_EXHAUSTED: &str = "RETRIES_EXHAUSTED";
pub const FAILURE_VENUE_REJECTED: &str = "VENUE_REJECTED";
pub const FAILURE_RECONCILIATION_FAILED: &str = "RECONCILIATION_FAILED";

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
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SwapOrder {
    pub id: Uuid,
    pub status: String,
    pub venue: String,
    pub source_asset: String,
    pub dest_asset: String,
    pub amount: BigDecimal,
    pub filled_amount: BigDecimal,
    pub attempts: i32,
    /// Idempotency key of the originating hand-off (`SwapHandoff::idempotency_key`).
    pub idempotency_key: String,
    /// The Phase 1 transaction this order swaps.
    pub transaction_id: Uuid,
    /// Quote the order was placed against, if one was issued.
    pub quote_id: Option<String>,
}

impl SwapOrder {
    /// Venue idempotency key, derived from the order ID so a crash
    /// mid-submission can never produce a second execution.
    pub fn idempotency_key(&self) -> String {
        format!("swap-order:{}", self.id)
    }
}

/// Domain transition guard. Same-state is a no-op and allowed.
pub fn validate_transition(from: &str, to: &str) -> Result<(), String> {
    if from == to || is_valid_transition(from, to, SWAP_ORDER_TRANSITIONS) {
        Ok(())
    } else {
        Err(format!("invalid swap order transition: {from} -> {to}"))
    }
}
