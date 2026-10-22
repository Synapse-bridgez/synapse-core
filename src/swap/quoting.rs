//! Quoting layer of the Phase 2 swap engine.
//!
//! Given a [`QuoteRequest`], [`QuoteEngine`] fans the request out to every
//! configured [`LiquiditySource`] in parallel, normalizes each response into a
//! [`Quote`], and selects the best route.
//!
//! # Selection rule
//!
//! 1. Each source is polled under its own timeout. A source that errors, times
//!    out, or returns an invalid response is logged and skipped; the remaining
//!    sources still produce a quote (graceful degradation).
//! 2. Quotes are ranked by [`Quote::net_output`] (`destination_amount - fee`),
//!    highest first. Ties go to the earlier source in configuration order.
//! 3. If the request carries a `reference_amount`, the best quote is
//!    hard-rejected with [`QuoteError::SlippageExceeded`] when its net output
//!    is below `reference_amount * (1 - max_slippage_bps / 10_000)`.
//!
//! Quotes expire `quote_ttl` after they are produced. Callers must call
//! [`Quote::ensure_unexpired`] before execution and re-quote on
//! [`QuoteError::Expired`].
//!
//! Persisting the selected quote against a swap order and the `Quoted`
//! transition are not handled here; this module only prices a swap.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::future::join_all;
use sqlx::types::BigDecimal;

use crate::config::SwapQuoteConfig;
use crate::error::QuoteError;
use crate::swap::sources::HttpLiquiditySource;

/// A request to price `amount` of `source_asset` into `destination_asset`.
#[derive(Debug, Clone, PartialEq)]
pub struct QuoteRequest {
    pub source_asset: String,
    pub destination_asset: String,
    /// Amount in `source_asset` units. Must be positive.
    pub amount: BigDecimal,
    /// Expected destination amount (for example from an indicative quote). When
    /// set, the best quote must be within the configured slippage tolerance of
    /// it.
    pub reference_amount: Option<BigDecimal>,
}

/// The raw price a [`LiquiditySource`] reports for a request.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceQuote {
    /// Gross destination amount, before fees.
    pub destination_amount: BigDecimal,
    /// Total cost to the caller in `destination_asset` units, including venue
    /// fees and any estimated network/gas cost.
    pub fee: BigDecimal,
}

/// A normalized, comparable quote produced by [`QuoteEngine`].
#[derive(Debug, Clone, PartialEq)]
pub struct Quote {
    /// Identifier of the [`LiquiditySource`] that produced this quote.
    pub source_id: String,
    pub source_asset: String,
    pub destination_asset: String,
    pub source_amount: BigDecimal,
    /// Gross destination amount, before fees.
    pub destination_amount: BigDecimal,
    /// Fee in `destination_asset` units.
    pub fee: BigDecimal,
    pub expires_at: DateTime<Utc>,
}

impl Quote {
    /// Destination amount the caller receives after fees. This is the ranking key.
    pub fn net_output(&self) -> BigDecimal {
        self.destination_amount.clone() - self.fee.clone()
    }

    /// Reject the quote if it has expired at `now`.
    pub fn ensure_unexpired(&self, now: DateTime<Utc>) -> Result<(), QuoteError> {
        if now >= self.expires_at {
            return Err(QuoteError::Expired);
        }
        Ok(())
    }
}

/// A venue or price feed that can quote a swap.
#[async_trait]
pub trait LiquiditySource: Send + Sync {
    /// Stable identifier for logs and for [`Quote::source_id`].
    fn id(&self) -> &str;

    /// Price `request` on this venue. Returns [`QuoteError::UnsupportedPair`]
    /// when the venue does not trade the pair.
    async fn quote(&self, request: &QuoteRequest) -> Result<SourceQuote, QuoteError>;
}

/// Tunables for [`QuoteEngine`]. Built from [`SwapQuoteConfig`].
#[derive(Debug, Clone)]
pub struct QuoteEngineConfig {
    pub source_timeout: Duration,
    pub quote_ttl: chrono::Duration,
    pub max_slippage_bps: u32,
}

impl From<&SwapQuoteConfig> for QuoteEngineConfig {
    fn from(config: &SwapQuoteConfig) -> Self {
        Self {
            source_timeout: Duration::from_millis(config.source_timeout_ms),
            quote_ttl: chrono::Duration::seconds(config.quote_ttl_secs as i64),
            max_slippage_bps: config.max_slippage_bps,
        }
    }
}

/// Aggregates quotes from several [`LiquiditySource`]s.
pub struct QuoteEngine {
    sources: Vec<Arc<dyn LiquiditySource>>,
    config: QuoteEngineConfig,
}

impl QuoteEngine {
    pub fn new(sources: Vec<Arc<dyn LiquiditySource>>, config: QuoteEngineConfig) -> Self {
        Self { sources, config }
    }

    /// Build an engine with one [`HttpLiquiditySource`] per configured endpoint.
    pub fn from_config(config: &SwapQuoteConfig) -> Self {
        let sources = config
            .liquidity_sources
            .iter()
            .map(|endpoint| {
                Arc::new(HttpLiquiditySource::new(
                    endpoint.id.clone(),
                    endpoint.url.clone(),
                    config.liquidity_api_key.clone(),
                )) as Arc<dyn LiquiditySource>
            })
            .collect();
        Self::new(sources, QuoteEngineConfig::from(config))
    }

    /// Price `request` across all sources and return the best normalized quote.
    pub async fn quote(&self, request: &QuoteRequest) -> Result<Quote, QuoteError> {
        validate_request(request)?;

        let now = Utc::now();
        let timeout = self.config.source_timeout;
        let outcomes = join_all(self.sources.iter().map(|source| async move {
            let outcome = match tokio::time::timeout(timeout, source.quote(request)).await {
                Ok(result) => result,
                Err(_) => Err(QuoteError::SourceTimeout(source.id().to_string())),
            };
            (source.id(), outcome)
        }))
        .await;

        let mut candidates = Vec::new();
        let mut unsupported = 0;
        for (source_id, outcome) in outcomes {
            match outcome {
                Ok(raw) => match self.normalize(source_id, request, raw, now) {
                    Ok(quote) => candidates.push(quote),
                    Err(err) => {
                        tracing::warn!(source = source_id, error = %err, "discarding quote");
                    }
                },
                Err(QuoteError::UnsupportedPair) => unsupported += 1,
                Err(err) => {
                    tracing::warn!(source = source_id, error = %err, "liquidity source failed");
                }
            }
        }

        if candidates.is_empty() {
            let all_unsupported = !self.sources.is_empty() && unsupported == self.sources.len();
            return Err(if all_unsupported {
                QuoteError::UnsupportedPair
            } else {
                QuoteError::NoQuotes
            });
        }

        let best = candidates
            .into_iter()
            .reduce(|best, quote| {
                if quote.net_output() > best.net_output() {
                    quote
                } else {
                    best
                }
            })
            .ok_or(QuoteError::NoQuotes)?;

        if let Some(reference) = &request.reference_amount {
            let retained_bps = 10_000u32.saturating_sub(self.config.max_slippage_bps);
            let floor =
                reference.clone() * BigDecimal::from(retained_bps) / BigDecimal::from(10_000);
            if best.net_output() < floor {
                return Err(QuoteError::SlippageExceeded);
            }
        }

        Ok(best)
    }

    /// Turn a source's raw price into a [`Quote`], rejecting nonsensical values.
    fn normalize(
        &self,
        source_id: &str,
        request: &QuoteRequest,
        raw: SourceQuote,
        now: DateTime<Utc>,
    ) -> Result<Quote, QuoteError> {
        let zero = BigDecimal::from(0);
        if raw.destination_amount < zero || raw.fee < zero || raw.fee > raw.destination_amount {
            return Err(QuoteError::InvalidSourceQuote(source_id.to_string()));
        }

        Ok(Quote {
            source_id: source_id.to_string(),
            source_asset: request.source_asset.clone(),
            destination_asset: request.destination_asset.clone(),
            source_amount: request.amount.clone(),
            destination_amount: raw.destination_amount,
            fee: raw.fee,
            expires_at: now + self.config.quote_ttl,
        })
    }
}

fn validate_request(request: &QuoteRequest) -> Result<(), QuoteError> {
    if request.amount <= BigDecimal::from(0) {
        return Err(QuoteError::InvalidAmount);
    }
    let source = request.source_asset.trim();
    let destination = request.destination_asset.trim();
    if source.is_empty() || destination.is_empty() || source == destination {
        return Err(QuoteError::InvalidAssetPair);
    }
    Ok(())
}
