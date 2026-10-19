//! Quoting engine: fans out to price sources, applies source spread and the
//! platform fee, ranks the resulting routes, and persists the issued quote
//! with a TTL.
//!
//! # Rounding policy
//!
//! All amounts are in quote-asset units at [`QUOTE_SCALE`] decimal places.
//! Every rounding step favours the platform, never the user:
//!
//! - `gross = floor(amount * price)` - the user never receives more than the
//!   exact product.
//! - `spread = ceil(gross * spread_bps / 10000)` - the platform keeps any
//!   sub-unit remainder.
//! - `fee = ceil((gross - spread) * fee_bps / 10000)` - same rule for the fee.
//! - `net = gross - spread - fee` - exact at `QUOTE_SCALE`, so no residue is
//!   lost or invented.
//!
//! # Ranking
//!
//! Candidates are ranked by `net_amount`, highest first. The sort is stable,
//! so equal net amounts keep the configured source order, which is the
//! tie-break precedence.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bigdecimal::RoundingMode;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use futures::future::join_all;
use serde::{Deserialize, Serialize};
use sqlx::types::BigDecimal;
use sqlx::PgPool;
use uuid::Uuid;

use crate::config::SwapQuoteConfig;
use crate::db::queries;
use crate::swap::error::QuoteError;
use crate::swap::market_data::{PriceSnapshot, PriceSource};

/// Decimal places kept on every quote amount (Stellar's 7-digit precision).
pub const QUOTE_SCALE: i64 = 7;

const BPS_DENOMINATOR: u32 = 10_000;

/// A request to price `amount` of `base_asset` into `quote_asset`.
#[derive(Debug, Clone, PartialEq)]
pub struct QuoteRequest {
    /// Caller-chosen idempotency key. Repeating a key returns the quote it
    /// already produced. Reusing it for a different pair or amount is rejected,
    /// and reusing it after that quote has expired fails with
    /// [`QuoteError::Expired`], so callers must use a fresh key to re-quote.
    pub request_key: String,
    pub base_asset: String,
    pub quote_asset: String,
    /// Amount of `base_asset` to sell.
    pub amount: BigDecimal,
}

/// One source's priced route, with the breakdown that produced its net output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuoteCandidate {
    pub source_id: String,
    pub price: BigDecimal,
    pub spread_bps: u32,
    pub captured_at: DateTime<Utc>,
    /// Output before spread and fee.
    pub gross_amount: BigDecimal,
    pub spread_amount: BigDecimal,
    pub fee_amount: BigDecimal,
    /// Amount of quote asset the user receives.
    pub net_amount: BigDecimal,
}

/// An issued, persisted quote.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwapQuote {
    pub id: Uuid,
    pub request_key: String,
    pub base_asset: String,
    pub quote_asset: String,
    pub amount: BigDecimal,
    pub fee_bps: u32,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// Ranked best-first, capped at `SWAP_MAX_QUOTE_CANDIDATES`.
    pub candidates: Vec<QuoteCandidate>,
}

impl SwapQuote {
    /// The winning route, i.e. the top-ranked candidate.
    pub fn winner(&self) -> Option<&QuoteCandidate> {
        self.candidates.first()
    }
}

/// Persistence boundary for issued quotes. Implemented by [`PgQuoteStore`];
/// tests supply in-memory fakes.
#[async_trait]
pub trait QuoteStore: Send + Sync {
    /// Persist `quote` unless its request key is already taken. Returns the
    /// quote stored for that key after the call, so concurrent inserts with
    /// the same key converge on one quote.
    async fn insert_or_get(&self, quote: &SwapQuote) -> Result<SwapQuote, QuoteError>;

    async fn get(&self, id: Uuid) -> Result<Option<SwapQuote>, QuoteError>;
}

/// Postgres-backed [`QuoteStore`] over the `swap_quotes` table.
#[derive(Clone)]
pub struct PgQuoteStore {
    pool: PgPool,
}

impl PgQuoteStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl QuoteStore for PgQuoteStore {
    async fn insert_or_get(&self, quote: &SwapQuote) -> Result<SwapQuote, QuoteError> {
        queries::insert_swap_quote_or_get(&self.pool, quote)
            .await
            .map_err(|e| QuoteError::Store(e.to_string()))
    }

    async fn get(&self, id: Uuid) -> Result<Option<SwapQuote>, QuoteError> {
        queries::get_swap_quote(&self.pool, id)
            .await
            .map_err(|e| QuoteError::Store(e.to_string()))
    }
}

/// Fans out to price sources, ranks routes, and persists quotes.
pub struct QuoteService {
    /// Ordered by precedence: on equal net output, the earlier source wins.
    sources: Vec<Arc<dyn PriceSource>>,
    store: Arc<dyn QuoteStore>,
    config: SwapQuoteConfig,
}

impl QuoteService {
    pub fn new(
        sources: Vec<Arc<dyn PriceSource>>,
        store: Arc<dyn QuoteStore>,
        config: SwapQuoteConfig,
    ) -> Self {
        Self {
            sources,
            store,
            config,
        }
    }

    /// Price `request`, persist the winning quote, and return it.
    ///
    /// A source that errors, times out, reports a stale snapshot, or yields no
    /// positive net output is skipped. Only if no source produces a usable
    /// route does the request fail.
    pub async fn quote(
        &self,
        request: &QuoteRequest,
        now: DateTime<Utc>,
    ) -> Result<SwapQuote, QuoteError> {
        self.validate(request)?;

        let supporting: Vec<&Arc<dyn PriceSource>> = self
            .sources
            .iter()
            .filter(|s| s.supports(&request.base_asset, &request.quote_asset))
            .collect();
        if supporting.is_empty() {
            return Err(QuoteError::UnsupportedPair {
                base: request.base_asset.clone(),
                quote: request.quote_asset.clone(),
            });
        }

        let timeout = Duration::from_millis(self.config.source_timeout_ms);
        let base = request.base_asset.as_str();
        let quote_asset = request.quote_asset.as_str();
        let fetches = supporting.iter().map(|source| async move {
            match tokio::time::timeout(timeout, source.snapshot(base, quote_asset)).await {
                Ok(Ok(snapshot)) => Some((source.id().to_string(), snapshot)),
                Ok(Err(e)) => {
                    tracing::warn!(source = source.id(), error = %e, "swap price source failed");
                    None
                }
                Err(_) => {
                    tracing::warn!(source = source.id(), "swap price source timed out");
                    None
                }
            }
        });

        // `join_all` preserves input order, so precedence survives the fan-out.
        let mut candidates: Vec<QuoteCandidate> = join_all(fetches)
            .await
            .into_iter()
            .flatten()
            .filter_map(|(source_id, snapshot)| {
                self.price_candidate(source_id, &snapshot, &request.amount)
            })
            .collect();
        if candidates.is_empty() {
            return Err(QuoteError::NoLiquidity {
                base: request.base_asset.clone(),
                quote: request.quote_asset.clone(),
            });
        }

        candidates.sort_by(|a, b| b.net_amount.cmp(&a.net_amount));
        candidates.truncate(self.config.max_candidates);

        let ttl = ChronoDuration::seconds(self.config.quote_ttl_seconds as i64);
        let issued = SwapQuote {
            id: Uuid::new_v4(),
            request_key: request.request_key.clone(),
            base_asset: request.base_asset.clone(),
            quote_asset: request.quote_asset.clone(),
            amount: request.amount.clone(),
            fee_bps: self.config.fee_bps,
            issued_at: now,
            expires_at: now + ttl,
            candidates,
        };

        let stored = self.store.insert_or_get(&issued).await?;
        if stored.base_asset != request.base_asset
            || stored.quote_asset != request.quote_asset
            || stored.amount != request.amount
        {
            return Err(QuoteError::RequestKeyConflict(request.request_key.clone()));
        }
        Ok(stored)
    }

    /// Load a quote for execution. Unknown quotes fail with
    /// [`QuoteError::NotFound`]; quotes at or past `expires_at` fail with
    /// [`QuoteError::Expired`].
    pub async fn ensure_executable(
        &self,
        id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<SwapQuote, QuoteError> {
        let quote = self
            .store
            .get(id)
            .await?
            .ok_or(QuoteError::NotFound(id))?;
        if now >= quote.expires_at {
            return Err(QuoteError::Expired(id));
        }
        Ok(quote)
    }

    fn validate(&self, request: &QuoteRequest) -> Result<(), QuoteError> {
        if request.base_asset == request.quote_asset {
            return Err(QuoteError::SameAsset(request.base_asset.clone()));
        }
        let zero = BigDecimal::from(0);
        if request.amount <= zero {
            return Err(QuoteError::InvalidAmount(request.amount.to_string()));
        }
        if request.amount < self.config.min_order_size
            || request.amount > self.config.max_order_size
        {
            return Err(QuoteError::OrderSizeOutOfBounds {
                amount: request.amount.to_string(),
                min: self.config.min_order_size.to_string(),
                max: self.config.max_order_size.to_string(),
            });
        }
        Ok(())
    }

    /// Apply spread and fee to one snapshot. Returns `None` for snapshots that
    /// are stale, malformed, or produce no positive net output.
    fn price_candidate(
        &self,
        source_id: String,
        snapshot: &PriceSnapshot,
        amount: &BigDecimal,
    ) -> Option<QuoteCandidate> {
        if snapshot.stale {
            tracing::warn!(source = %source_id, "skipping stale swap price snapshot");
            return None;
        }
        let zero = BigDecimal::from(0);
        if snapshot.price <= zero || snapshot.spread_bps > BPS_DENOMINATOR {
            tracing::warn!(source = %source_id, "skipping malformed swap price snapshot");
            return None;
        }

        let gross_amount =
            (amount * &snapshot.price).with_scale_round(QUOTE_SCALE, RoundingMode::Down);
        let spread_amount = bps_deduction(&gross_amount, snapshot.spread_bps);
        let fee_amount = bps_deduction(&(&gross_amount - &spread_amount), self.config.fee_bps);
        let net_amount = &gross_amount - &spread_amount - &fee_amount;
        if net_amount <= zero {
            return None;
        }

        Some(QuoteCandidate {
            source_id,
            price: snapshot.price.clone(),
            spread_bps: snapshot.spread_bps,
            captured_at: snapshot.captured_at,
            gross_amount,
            spread_amount,
            fee_amount,
            net_amount,
        })
    }
}

/// `amount * bps / 10000`, rounded up to [`QUOTE_SCALE`]. Used for every
/// deduction so the platform keeps any sub-unit remainder.
fn bps_deduction(amount: &BigDecimal, bps: u32) -> BigDecimal {
    (amount * BigDecimal::from(bps) / BigDecimal::from(BPS_DENOMINATOR))
        .with_scale_round(QUOTE_SCALE, RoundingMode::Ceiling)
}
