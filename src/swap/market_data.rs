//! Price source boundary consumed by the quoting engine.
//!
//! Concrete venues (DEXes, order books, anchors) implement [`PriceSource`].
//! The quoting engine never talks to a venue directly, so tests can supply
//! fakes.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::types::BigDecimal;

/// A point-in-time price observation from one liquidity source.
#[derive(Debug, Clone, PartialEq)]
pub struct PriceSnapshot {
    /// Units of quote asset per one unit of base asset, before spread and fee.
    /// Must be strictly positive.
    pub price: BigDecimal,
    /// Source-specific spread in basis points, deducted from gross output.
    /// Must be within `0..=10000`.
    pub spread_bps: u32,
    /// When the source observed this price.
    pub captured_at: DateTime<Utc>,
    /// Set by the source when it knows this snapshot is out of date.
    /// Stale snapshots are never quoted.
    pub stale: bool,
}

/// A liquidity source that can price a pair.
///
/// Sources are queried concurrently and in isolation: an error from one
/// source is recorded and skipped, never fatal to the whole quote.
#[async_trait]
pub trait PriceSource: Send + Sync {
    /// Stable identifier used in quote audit output (e.g. `"horizon-sdex"`).
    fn id(&self) -> &str;

    /// Whether this source can price `base`/`quote` at all.
    fn supports(&self, base: &str, quote: &str) -> bool;

    /// Fetch the current snapshot for `base`/`quote`. Errors are free-form
    /// strings because they are only surfaced in logs.
    async fn snapshot(&self, base: &str, quote: &str) -> Result<PriceSnapshot, String>;
}
