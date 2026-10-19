//! Error type for the Phase 2 swap engine.
//!
//! Follows the conventions in [`crate::error`]: every variant maps to a stable
//! `ERR_SWAP_*` code and HTTP status in `AppError`, and `SwapError` reaches the
//! crate-level error type through `AppError::Swap`. Because `Internal` carries
//! free-form engine text, it is redacted from client responses like the other
//! wrapping variants.

use thiserror::Error;

/// Error returned by a [`SwapEngine`](super::SwapEngine) or by any swap
/// submodule boundary trait ([`QuoteSource`](super::quote::QuoteSource),
/// [`ExecutionVenue`](super::execution::ExecutionVenue),
/// [`SettlementAdapter`](super::settlement::SettlementAdapter)).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SwapError {
    /// The engine is unreachable or not ready.
    #[error("swap engine unavailable")]
    Unavailable,
    /// The hand-off is malformed or references data the engine cannot use.
    #[error("invalid swap hand-off: {0}")]
    InvalidHandoff(String),
    /// The engine failed internally.
    #[error("swap engine internal error: {0}")]
    Internal(String),
}
