//! Risk gating: deciding whether a [`SwapOrder`] may proceed to execution.
//!
//! Limits applied here (maximum order amount, fee) come from `SwapConfig`.
//! This module currently defines only the decision type; the checks themselves
//! arrive with the first execution-capable issue.

/// Outcome of a risk check on a swap order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RiskDecision {
    /// The order may proceed to execution.
    Approve,
    /// The order must not be executed. `reason` is safe to log, not to show
    /// to clients.
    Reject { reason: String },
}
