//! Quoting: pricing a source-asset amount into a target asset.
//!
//! Defines the [`QuoteSource`] boundary and its request/response types. Quotes
//! are valid until `expires_at`, which is driven by `SWAP_QUOTE_TTL_SECONDS`
//! (see `SwapConfig`). No pricing logic lives here yet.

use async_trait::async_trait;
use sqlx::types::BigDecimal;

use super::error::SwapError;

/// A request to price `amount` of `source_asset` into `target_asset`.
#[derive(Debug, Clone, PartialEq)]
pub struct QuoteRequest {
    pub source_asset: String,
    pub target_asset: String,
    pub amount: BigDecimal,
}

/// A priced quote returned by a [`QuoteSource`].
#[derive(Debug, Clone, PartialEq)]
pub struct Quote {
    /// Identifier the quote is referenced by when an order is placed.
    pub quote_id: String,
    pub source_asset: String,
    pub target_asset: String,
    pub amount_in: BigDecimal,
    pub amount_out: BigDecimal,
    /// After this instant the quote must not be used to place an order.
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

/// Source of prices for swap requests (an in-process pricer, an external
/// price feed, an on-chain AMM, ...). Implementations are wired in later
/// Phase 2 issues.
#[async_trait]
pub trait QuoteSource: Send + Sync {
    /// Price a request, or report why no quote can be issued.
    async fn quote(&self, request: &QuoteRequest) -> Result<Quote, SwapError>;
}
