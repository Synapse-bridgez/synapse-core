//! Normalized price snapshots and the source trait that feeds the quoting engine.
//!
//! A [`PriceSnapshot`] is the single internal notion of a price: an exact
//! decimal mid (with optional bid/ask), the source that produced it, and two
//! timestamps. `source_timestamp` is when the upstream observed the price and
//! drives staleness; `ingested_at` is when this service accepted it.
//!
//! This module decides nothing about routes, spreads, or fees. It only answers
//! "what is the price, how old is it, and where did it come from".

use async_trait::async_trait;
use bigdecimal::BigDecimal;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::market_data::MarketDataError;

/// A directed asset pair, e.g. `XLM/USD` means the price of one XLM in USD.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AssetPair {
    pub base: String,
    pub quote: String,
}

impl AssetPair {
    pub fn new(base: impl Into<String>, quote: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            quote: quote.into(),
        }
    }

    /// Canonical `BASE/QUOTE` form used for cache keys and logs.
    pub fn key(&self) -> String {
        format!("{}/{}", self.base, self.quote)
    }
}

/// A normalized, timestamped price for one [`AssetPair`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceSnapshot {
    pub pair: AssetPair,
    /// Mid price, as an exact decimal. Always positive.
    pub mid: BigDecimal,
    pub bid: Option<BigDecimal>,
    pub ask: Option<BigDecimal>,
    /// Identifier of the source adapter that produced this snapshot.
    pub source: String,
    /// When the upstream observed the price. Staleness is measured from here.
    pub source_timestamp: DateTime<Utc>,
    /// When this service accepted the price.
    pub ingested_at: DateTime<Utc>,
    /// `true` only when this snapshot was served from the last-known fallback
    /// because the source was unavailable. Never set on a fresh fetch.
    #[serde(default)]
    pub stale: bool,
}

impl PriceSnapshot {
    /// Seconds between the source observation and `now`, clamped at zero so a
    /// slightly-future timestamp (within clock-skew tolerance) reads as fresh.
    pub fn age_secs(&self, now: DateTime<Utc>) -> i64 {
        (now - self.source_timestamp).num_seconds().max(0)
    }

    /// Whether this snapshot may price a quote: it must not be a fallback copy
    /// and must be no older than `max_age_secs`.
    pub fn is_usable(&self, now: DateTime<Utc>, max_age_secs: i64) -> bool {
        !self.stale && self.age_secs(now) <= max_age_secs
    }
}

/// An upstream price source. Implementations must be safe to call concurrently
/// and must return within their configured timeouts.
#[async_trait]
pub trait PriceSource: Send + Sync {
    /// Identifier stamped onto every snapshot this source produces.
    fn name(&self) -> &str;

    /// Fetch and normalize the current price for `pair`.
    async fn fetch(&self, pair: &AssetPair) -> Result<PriceSnapshot, MarketDataError>;
}
