//! Typed errors for the swap quoting engine.

use std::fmt;

use uuid::Uuid;

/// Error returned by the quoting engine in `src/swap/quote.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuoteError {
    /// Base and quote assets are the same, so there is nothing to swap.
    SameAsset(String),
    /// No configured price source supports this asset pair.
    UnsupportedPair { base: String, quote: String },
    /// The requested amount is zero or negative.
    InvalidAmount(String),
    /// The requested amount falls outside the configured min/max order size.
    OrderSizeOutOfBounds {
        amount: String,
        min: String,
        max: String,
    },
    /// Every supporting source failed, timed out, returned stale data, or
    /// produced no positive net output.
    NoLiquidity { base: String, quote: String },
    /// The request key was already used for a quote with different parameters.
    RequestKeyConflict(String),
    /// No quote exists with the given id.
    NotFound(Uuid),
    /// The quote's TTL has elapsed and it can no longer be executed.
    Expired(Uuid),
    /// The quote store failed.
    Store(String),
}

impl fmt::Display for QuoteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            QuoteError::SameAsset(asset) => {
                write!(f, "base and quote asset are both {asset}")
            }
            QuoteError::UnsupportedPair { base, quote } => {
                write!(f, "unsupported pair {base}/{quote}")
            }
            QuoteError::InvalidAmount(amount) => {
                write!(f, "invalid order amount {amount}: must be positive")
            }
            QuoteError::OrderSizeOutOfBounds { amount, min, max } => {
                write!(f, "order size {amount} outside bounds [{min}, {max}]")
            }
            QuoteError::NoLiquidity { base, quote } => {
                write!(f, "no usable liquidity for {base}/{quote}")
            }
            QuoteError::RequestKeyConflict(key) => {
                write!(f, "request key {key} already used for a different quote")
            }
            QuoteError::NotFound(id) => write!(f, "quote {id} not found"),
            QuoteError::Expired(id) => write!(f, "quote {id} has expired"),
            QuoteError::Store(m) => write!(f, "quote store error: {m}"),
        }
    }
}

impl std::error::Error for QuoteError {}
