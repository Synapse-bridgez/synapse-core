//! The Swap Engine service: quote → execute → settle or compensate.
//!
//! * [`SwapService::quote`] asks the [`Router`] for the best (possibly
//!   multi-hop) route and stores a [`Quote`] that **includes that route**.
//! * [`SwapService::execute`] runs the quote's route leg by leg, feeding
//!   each leg's actual fill into the next. It never re-routes: what the
//!   client was quoted is what executes.
//! * If a leg fails, every leg already filled is reversed in reverse order
//!   (compensation) and the swap ends `compensated` (or
//!   `compensation_failed` if a reversal itself failed, which needs
//!   operator attention).
//! * Executing the same quote twice returns the original swap record.
//! * Client [`ProtectionParams`] (`min_received`, `max_slippage_bps`) are
//!   turned into a single minimum output (the *floor*) measured against a
//!   [`PriceOracle`] reference rate. A quote whose route cannot clear the
//!   floor is refused; an execution that drifts below it is stopped before
//!   the next leg and compensated. See [`crate::swap::protection`].
//!
//! Every state change is persisted to the [`SwapStore`] and appended to the
//! swap's event log.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::swap::asset::{Asset, AssetRegistry};
use crate::swap::decimal::{Decimal, DecimalError};
use crate::swap::liquidity::{LegFill, LegOrder, LiquidityError, LiquidityProvider};
use crate::swap::oracle::PriceOracle;
use crate::swap::pricing::implied_rate;
use crate::swap::protection::{
    floor, price_impact, projected_output, proportional_floor, PriceImpact, ProtectionParams,
};
use crate::swap::rounding::{Purpose, RoundingPolicy};
use crate::swap::routing::{Route, Router, RouterConfig, RoutingError};
use crate::swap::runtime::{Clock, IdGenerator, SystemClock, UuidIdGenerator};
use crate::swap::store::{InMemorySwapStore, StoreError, SwapStore};

/// Default lifetime of a quote.
pub const DEFAULT_QUOTE_TTL_SECS: i64 = 30;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteRequest {
    pub source_asset: String,
    pub destination_asset: String,
    /// Amount of `source_asset` to sell, as a decimal string.
    pub amount_in: Decimal,
    /// Optional minimum-received / max-slippage protection. Checked at
    /// quote time and carried into execution.
    #[serde(default)]
    pub protection: ProtectionParams,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quote {
    pub quote_id: String,
    pub source_asset: String,
    pub destination_asset: String,
    pub amount_in: Decimal,
    pub expected_out: Decimal,
    /// Effective end-to-end rate (`expected_out / amount_in`), display only.
    pub rate: Decimal,
    /// The route that will be executed.
    pub route: Route,
    pub candidates_considered: usize,
    pub candidates_priced: usize,
    /// Validated protection requested with the quote.
    #[serde(default, skip_serializing_if = "ProtectionParams::is_empty")]
    pub protection: ProtectionParams,
    /// Oracle reference rate at quote time, when an oracle is configured.
    pub reference_rate: Option<Decimal>,
    /// Price impact of `expected_out` versus the reference rate, in signed
    /// bps (positive = worse than reference).
    pub price_impact_bps: Option<Decimal>,
    /// Effective minimum output (floor) at quote time, when protected.
    pub min_out: Option<Decimal>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// A swap order: execute a stored quote, optionally with tighter
/// protection than the quote carried.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecuteRequest {
    pub quote_id: String,
    /// Tightens (never loosens) the quote's protection.
    #[serde(default)]
    pub protection: ProtectionParams,
}

impl ExecuteRequest {
    pub fn new(quote_id: impl Into<String>) -> Self {
        Self {
            quote_id: quote_id.into(),
            protection: ProtectionParams::default(),
        }
    }

    pub fn with_protection(mut self, protection: ProtectionParams) -> Self {
        self.protection = protection;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwapStatus {
    Executing,
    Settled,
    Compensating,
    Compensated,
    CompensationFailed,
}

impl SwapStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            SwapStatus::Settled | SwapStatus::Compensated | SwapStatus::CompensationFailed
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegStatus {
    Pending,
    Filled,
    Failed,
    Skipped,
    Compensated,
    CompensationFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegRecord {
    pub index: usize,
    pub provider: String,
    pub from: String,
    pub to: String,
    /// Quoted input/output for this leg.
    pub quoted_in: Decimal,
    pub quoted_out: Decimal,
    /// Actual input/output once filled.
    pub amount_in: Option<Decimal>,
    pub amount_out: Option<Decimal>,
    pub fill_reference: Option<String>,
    pub status: LegStatus,
    pub error: Option<String>,
}

/// Why a swap did not settle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwapFailure {
    /// Machine-readable reason, e.g. `leg_failed`.
    pub code: String,
    pub message: String,
    /// Leg that triggered the failure, if any.
    pub leg_index: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwapRecord {
    pub swap_id: String,
    pub quote_id: String,
    pub status: SwapStatus,
    pub route: Route,
    pub amount_in: Decimal,
    pub expected_out: Decimal,
    /// Amount delivered, once settled.
    pub amount_out: Option<Decimal>,
    /// Effective protection (quote's, tightened by the order's).
    #[serde(default, skip_serializing_if = "ProtectionParams::is_empty")]
    pub protection: ProtectionParams,
    /// Oracle reference rate read at execution start.
    pub reference_rate: Option<Decimal>,
    /// Floor enforced during execution.
    pub min_out: Option<Decimal>,
    /// Realized price impact versus `reference_rate`, once settled.
    pub price_impact_bps: Option<Decimal>,
    pub legs: Vec<LegRecord>,
    pub failure: Option<SwapFailure>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwapEvent {
    pub swap_id: String,
    /// 1-based position in the swap's log (assigned by the store).
    pub seq: u64,
    pub at: DateTime<Utc>,
    pub kind: String,
    pub detail: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EngineError {
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error(transparent)]
    Amount(#[from] DecimalError),
    #[error(transparent)]
    Routing(#[from] RoutingError),
    #[error("quote {0} not found")]
    QuoteNotFound(String),
    #[error("swap {0} not found")]
    SwapNotFound(String),
    #[error("quote {quote_id} expired at {expired_at}")]
    QuoteExpired {
        quote_id: String,
        expired_at: DateTime<Utc>,
    },
    #[error("invalid protection: {0}")]
    InvalidProtection(String),
    #[error("route delivers {expected}, below min_received {min_received}")]
    MinReceivedUnattainable {
        expected: Decimal,
        min_received: Decimal,
    },
    #[error("price impact {impact_bps} bps exceeds max_slippage_bps {max_slippage_bps}")]
    PriceImpactExceeded {
        impact_bps: Decimal,
        max_slippage_bps: u32,
    },
    #[error("reference price unavailable: {0}")]
    ReferencePriceUnavailable(String),
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl EngineError {
    /// Stable machine-readable code for API responses.
    pub fn code(&self) -> &'static str {
        match self {
            EngineError::InvalidRequest(_) => "invalid_request",
            EngineError::Amount(_) => "invalid_amount",
            EngineError::Routing(RoutingError::UnknownAsset(_)) => "unknown_asset",
            EngineError::Routing(RoutingError::SameAsset) => "same_asset",
            EngineError::Routing(RoutingError::NonPositiveAmount) => "invalid_amount",
            EngineError::Routing(_) => "no_route",
            EngineError::QuoteNotFound(_) => "quote_not_found",
            EngineError::SwapNotFound(_) => "swap_not_found",
            EngineError::QuoteExpired { .. } => "quote_expired",
            EngineError::InvalidProtection(_) => "invalid_protection",
            EngineError::MinReceivedUnattainable { .. } => "min_received_unattainable",
            EngineError::PriceImpactExceeded { .. } => "price_impact_exceeded",
            EngineError::ReferencePriceUnavailable(_) => "reference_price_unavailable",
            EngineError::Store(_) => "store_error",
        }
    }

    /// HTTP status for API responses.
    pub fn http_status(&self) -> u16 {
        match self {
            EngineError::InvalidRequest(_)
            | EngineError::Amount(_)
            | EngineError::Routing(RoutingError::UnknownAsset(_))
            | EngineError::Routing(RoutingError::SameAsset)
            | EngineError::Routing(RoutingError::NonPositiveAmount)
            | EngineError::InvalidProtection(_) => 400,
            EngineError::Routing(_)
            | EngineError::MinReceivedUnattainable { .. }
            | EngineError::PriceImpactExceeded { .. } => 422,
            EngineError::ReferencePriceUnavailable(_) => 503,
            EngineError::QuoteNotFound(_) | EngineError::SwapNotFound(_) => 404,
            EngineError::QuoteExpired { .. } => 410,
            EngineError::Store(_) => 500,
        }
    }
}

pub struct SwapService {
    assets: AssetRegistry,
    providers: Vec<Arc<dyn LiquidityProvider>>,
    providers_by_id: BTreeMap<String, Arc<dyn LiquidityProvider>>,
    router: Router,
    oracle: Option<Arc<dyn PriceOracle>>,
    store: Arc<dyn SwapStore>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    quote_ttl: Duration,
    policy: RoundingPolicy,
}

pub struct SwapServiceBuilder {
    assets: AssetRegistry,
    providers: Vec<Arc<dyn LiquidityProvider>>,
    router_config: RouterConfig,
    oracle: Option<Arc<dyn PriceOracle>>,
    store: Option<Arc<dyn SwapStore>>,
    clock: Option<Arc<dyn Clock>>,
    ids: Option<Arc<dyn IdGenerator>>,
    quote_ttl: Duration,
    policy: RoundingPolicy,
}

impl SwapServiceBuilder {
    pub fn provider(mut self, p: Arc<dyn LiquidityProvider>) -> Self {
        self.providers.push(p);
        self
    }

    pub fn router_config(mut self, c: RouterConfig) -> Self {
        self.router_config = c;
        self
    }

    /// Reference-price source for price impact and slippage protection.
    /// Without one, `max_slippage_bps` requests fail closed with `503`.
    pub fn oracle(mut self, o: Arc<dyn PriceOracle>) -> Self {
        self.oracle = Some(o);
        self
    }

    pub fn store(mut self, s: Arc<dyn SwapStore>) -> Self {
        self.store = Some(s);
        self
    }

    pub fn clock(mut self, c: Arc<dyn Clock>) -> Self {
        self.clock = Some(c);
        self
    }

    pub fn ids(mut self, i: Arc<dyn IdGenerator>) -> Self {
        self.ids = Some(i);
        self
    }

    pub fn quote_ttl(mut self, ttl: Duration) -> Self {
        self.quote_ttl = ttl;
        self
    }

    pub fn build(self) -> SwapService {
        let providers_by_id = self
            .providers
            .iter()
            .map(|p| (p.id().to_string(), p.clone()))
            .collect();
        SwapService {
            assets: self.assets,
            providers: self.providers,
            providers_by_id,
            router: Router::new(self.router_config, self.policy),
            oracle: self.oracle,
            store: self
                .store
                .unwrap_or_else(|| Arc::new(InMemorySwapStore::new())),
            clock: self.clock.unwrap_or_else(|| Arc::new(SystemClock)),
            ids: self.ids.unwrap_or_else(|| Arc::new(UuidIdGenerator)),
            quote_ttl: self.quote_ttl,
            policy: self.policy,
        }
    }
}

impl SwapService {
    pub fn builder(assets: AssetRegistry) -> SwapServiceBuilder {
        SwapServiceBuilder {
            assets,
            providers: Vec::new(),
            router_config: RouterConfig::default(),
            oracle: None,
            store: None,
            clock: None,
            ids: None,
            quote_ttl: Duration::seconds(DEFAULT_QUOTE_TTL_SECS),
            policy: RoundingPolicy::STANDARD,
        }
    }

    pub fn assets(&self) -> &AssetRegistry {
        &self.assets
    }

    /// Price a swap and store the quote (including its route).
    pub async fn quote(&self, req: QuoteRequest) -> Result<Quote, EngineError> {
        let src = self
            .assets
            .get(&req.source_asset)
            .ok_or_else(|| RoutingError::UnknownAsset(req.source_asset.clone()))?;
        let amount_in = src.normalize(&req.amount_in)?;
        let dst = self
            .assets
            .get(&req.destination_asset)
            .ok_or_else(|| RoutingError::UnknownAsset(req.destination_asset.clone()))?;
        let protection = req
            .protection
            .validated(dst)
            .map_err(|e| EngineError::InvalidProtection(e.to_string()))?;
        let selection = self
            .router
            .best_route(
                &self.providers,
                &self.assets,
                &req.source_asset,
                &req.destination_asset,
                &amount_in,
            )
            .await?;
        let route = selection.route;

        let reference_rate = self
            .reference_rate(src, dst, protection.max_slippage_bps.is_some())
            .await?;
        let impact = self.impact(
            &amount_in,
            &route.expected_out,
            reference_rate.as_ref(),
            dst,
        )?;
        let min_out = floor(
            &protection,
            &amount_in,
            reference_rate.as_ref(),
            dst,
            &self.policy,
        );
        // Short-circuit: don't hand out a quote that can't satisfy its own
        // protection.
        Self::check_clears_floor(
            &protection,
            min_out.as_ref(),
            &route.expected_out,
            impact.as_ref(),
        )?;

        let now = self.clock.now();
        let quote = Quote {
            quote_id: self.ids.next_id("quote"),
            source_asset: req.source_asset,
            destination_asset: req.destination_asset,
            rate: implied_rate(&amount_in, &route.expected_out, &self.policy)?,
            amount_in,
            expected_out: route.expected_out.clone(),
            route,
            candidates_considered: selection.candidates_considered,
            candidates_priced: selection.candidates_priced,
            protection,
            reference_rate,
            price_impact_bps: impact.map(|i| i.impact_bps),
            min_out,
            created_at: now,
            expires_at: now + self.quote_ttl,
        };
        self.store.insert_quote(&quote).await?;
        Ok(quote)
    }

    /// Reference rate from the oracle. With `required`, a missing oracle or
    /// oracle failure is an error (fail closed); otherwise it is `None`.
    async fn reference_rate(
        &self,
        from: &Asset,
        to: &Asset,
        required: bool,
    ) -> Result<Option<Decimal>, EngineError> {
        let Some(oracle) = &self.oracle else {
            return if required {
                Err(EngineError::ReferencePriceUnavailable(
                    "no price oracle configured".into(),
                ))
            } else {
                Ok(None)
            };
        };
        match oracle.reference_rate(from, to).await {
            Ok(rate) => Ok(Some(rate)),
            Err(e) if required => Err(EngineError::ReferencePriceUnavailable(e.to_string())),
            Err(e) => {
                tracing::warn!(oracle = oracle.id(), error = %e, "reference price unavailable; quoting without price impact");
                Ok(None)
            }
        }
    }

    fn impact(
        &self,
        amount_in: &Decimal,
        out: &Decimal,
        reference_rate: Option<&Decimal>,
        dst: &Asset,
    ) -> Result<Option<PriceImpact>, EngineError> {
        reference_rate
            .map(|r| price_impact(amount_in, out, r, dst, &self.policy))
            .transpose()
            .map_err(EngineError::from)
    }

    /// Error if `expected` is below the protection floor, naming whichever
    /// parameter set the floor.
    fn check_clears_floor(
        protection: &ProtectionParams,
        min_out: Option<&Decimal>,
        expected: &Decimal,
        impact: Option<&PriceImpact>,
    ) -> Result<(), EngineError> {
        let Some(min_out) = min_out else {
            return Ok(());
        };
        if expected >= min_out {
            return Ok(());
        }
        if Self::min_received_binds(protection, min_out) {
            return Err(EngineError::MinReceivedUnattainable {
                expected: expected.clone(),
                min_received: min_out.clone(),
            });
        }
        Err(EngineError::PriceImpactExceeded {
            impact_bps: impact
                .map(|i| i.impact_bps.clone())
                .unwrap_or_else(|| Decimal::zero(0)),
            max_slippage_bps: protection.max_slippage_bps.unwrap_or_default(),
        })
    }

    fn min_received_binds(protection: &ProtectionParams, min_out: &Decimal) -> bool {
        protection.min_received.as_ref() == Some(min_out)
    }

    fn protection_failure(
        protection: &ProtectionParams,
        min_out: &Decimal,
        leg_index: usize,
        message: String,
    ) -> SwapFailure {
        let code = if Self::min_received_binds(protection, min_out) {
            "min_received_not_met"
        } else {
            "slippage_exceeded"
        };
        SwapFailure {
            code: code.into(),
            message,
            leg_index: Some(leg_index),
        }
    }

    pub async fn get_quote(&self, quote_id: &str) -> Result<Quote, EngineError> {
        self.store
            .get_quote(quote_id)
            .await?
            .ok_or_else(|| EngineError::QuoteNotFound(quote_id.to_string()))
    }

    pub async fn get_swap(&self, swap_id: &str) -> Result<SwapRecord, EngineError> {
        self.store
            .get_swap(swap_id)
            .await?
            .ok_or_else(|| EngineError::SwapNotFound(swap_id.to_string()))
    }

    pub async fn events(&self, swap_id: &str) -> Result<Vec<SwapEvent>, EngineError> {
        self.get_swap(swap_id).await?;
        Ok(self.store.events(swap_id).await?)
    }

    /// Execute a stored quote along its route.
    pub async fn execute(&self, req: ExecuteRequest) -> Result<SwapRecord, EngineError> {
        let quote = self.get_quote(&req.quote_id).await?;
        if let Some(existing) = self.store.get_swap_by_quote(&quote.quote_id).await? {
            return Ok(existing);
        }
        let now = self.clock.now();
        if now >= quote.expires_at {
            return Err(EngineError::QuoteExpired {
                quote_id: quote.quote_id,
                expired_at: quote.expires_at,
            });
        }

        let (src, dst) = match (
            self.assets.get(&quote.source_asset),
            self.assets.get(&quote.destination_asset),
        ) {
            (Some(s), Some(d)) => (s, d),
            _ => {
                return Err(EngineError::InvalidRequest(
                    "quote references an asset that is no longer registered".into(),
                ))
            }
        };
        let order_protection = req
            .protection
            .validated(dst)
            .map_err(|e| EngineError::InvalidProtection(e.to_string()))?;
        let protection = quote.protection.tightened_by(&order_protection);
        // Re-read the reference so the floor reflects the market now, not
        // at quote time.
        let reference_rate = self
            .reference_rate(src, dst, protection.max_slippage_bps.is_some())
            .await?;
        let min_out = floor(
            &protection,
            &quote.amount_in,
            reference_rate.as_ref(),
            dst,
            &self.policy,
        );
        let impact = self.impact(
            &quote.amount_in,
            &quote.expected_out,
            reference_rate.as_ref(),
            dst,
        )?;
        // Short-circuit before any leg runs if the quoted route can no
        // longer clear the floor.
        Self::check_clears_floor(
            &protection,
            min_out.as_ref(),
            &quote.expected_out,
            impact.as_ref(),
        )?;

        let mut swap = SwapRecord {
            swap_id: self.ids.next_id("swap"),
            quote_id: quote.quote_id.clone(),
            status: SwapStatus::Executing,
            route: quote.route.clone(),
            amount_in: quote.amount_in.clone(),
            expected_out: quote.expected_out.clone(),
            amount_out: None,
            protection,
            reference_rate,
            min_out,
            price_impact_bps: None,
            legs: quote
                .route
                .hops
                .iter()
                .enumerate()
                .map(|(index, h)| LegRecord {
                    index,
                    provider: h.provider.clone(),
                    from: h.from.code.clone(),
                    to: h.to.code.clone(),
                    quoted_in: h.amount_in.clone(),
                    quoted_out: h.expected_out.clone(),
                    amount_in: None,
                    amount_out: None,
                    fill_reference: None,
                    status: LegStatus::Pending,
                    error: None,
                })
                .collect(),
            failure: None,
            created_at: now,
            updated_at: now,
        };
        match self.store.insert_swap(&swap).await {
            Ok(()) => {}
            Err(StoreError::DuplicateSwapForQuote(_)) => {
                // Lost a race with a concurrent execute of the same quote.
                if let Some(existing) = self.store.get_swap_by_quote(&quote.quote_id).await? {
                    return Ok(existing);
                }
                return Err(StoreError::DuplicateSwapForQuote(quote.quote_id).into());
            }
            Err(e) => return Err(e.into()),
        }
        self.event(
            &swap,
            "swap.executing",
            serde_json::json!({
                "quote_id": swap.quote_id,
                "route": swap.route.key(),
                "min_out": swap.min_out,
                "reference_rate": swap.reference_rate,
            }),
        )
        .await?;

        self.run_legs(&mut swap).await?;
        Ok(swap)
    }

    async fn run_legs(&self, swap: &mut SwapRecord) -> Result<(), EngineError> {
        let mut running = swap.amount_in.clone();
        let mut fills: Vec<(usize, LegFill)> = Vec::new();
        let hops = swap.route.hops.clone();
        let last = hops.len().saturating_sub(1);
        let route_out = swap.route.expected_out.clone();
        let dst_decimals = hops.last().map(|h| h.to.decimals).unwrap_or_default();
        let min_out = swap.min_out.clone();
        let protection = swap.protection.clone();
        for (i, hop) in hops.iter().enumerate() {
            let Some(provider) = self.providers_by_id.get(&hop.provider).cloned() else {
                let failure = SwapFailure {
                    code: "provider_missing".into(),
                    message: format!("provider {} is not registered", hop.provider),
                    leg_index: Some(i),
                };
                swap.legs[i].status = LegStatus::Failed;
                swap.legs[i].error = Some(failure.message.clone());
                return self.compensate(swap, fills, failure).await;
            };
            // Final leg must clear the floor; earlier legs get their
            // proportional share of it so providers can fail fast.
            let leg_min = match &min_out {
                Some(f) if i == last => Some(f.clone()),
                Some(f) => Some(proportional_floor(
                    f,
                    &hop.expected_out,
                    &route_out,
                    hop.to.decimals,
                    &self.policy,
                )?),
                None => None,
            };
            let order = LegOrder {
                swap_id: swap.swap_id.clone(),
                leg_index: i,
                from: hop.from.clone(),
                to: hop.to.clone(),
                amount_in: running.clone(),
                min_out: leg_min,
            };
            match provider.execute(&order).await {
                Ok(fill) => {
                    let out = fill
                        .amount_out
                        .rescale(hop.to.decimals, self.policy.mode(Purpose::Fill));
                    if !out.is_positive() {
                        let failure = SwapFailure {
                            code: "empty_fill".into(),
                            message: format!("leg {i} filled for zero"),
                            leg_index: Some(i),
                        };
                        swap.legs[i].status = LegStatus::Failed;
                        swap.legs[i].error = Some(failure.message.clone());
                        fills.push((i, fill));
                        return self.compensate(swap, fills, failure).await;
                    }
                    let leg = &mut swap.legs[i];
                    leg.status = LegStatus::Filled;
                    leg.amount_in = Some(running.clone());
                    leg.amount_out = Some(out.clone());
                    leg.fill_reference = Some(fill.reference.clone());
                    swap.updated_at = self.clock.now();
                    self.store.update_swap(swap).await?;
                    self.event(
                        swap,
                        "leg.filled",
                        serde_json::json!({
                            "leg": i,
                            "provider": hop.provider,
                            "amount_in": running,
                            "amount_out": out,
                            "reference": fill.reference,
                        }),
                    )
                    .await?;
                    running = out;
                    fills.push((i, fill));

                    // Protection: project the final output from what this
                    // leg actually delivered; stop before the next leg if
                    // it can no longer clear the floor.
                    if let Some(f) = &min_out {
                        let projected = if i == last {
                            running.clone()
                        } else {
                            projected_output(
                                &running,
                                &hop.expected_out,
                                &route_out,
                                dst_decimals,
                                &self.policy,
                            )?
                        };
                        if &projected < f {
                            let failure = Self::protection_failure(
                                &protection,
                                f,
                                i,
                                format!(
                                    "after leg {i} the swap would deliver {projected}, below the minimum {f}"
                                ),
                            );
                            self.event(
                                swap,
                                "protection.tripped",
                                serde_json::json!({
                                    "leg": i,
                                    "projected_out": projected,
                                    "min_out": f,
                                    "code": failure.code,
                                }),
                            )
                            .await?;
                            return self.compensate(swap, fills, failure).await;
                        }
                    }
                }
                Err(e) => {
                    let failure = match (&e, &min_out) {
                        (LiquidityError::MinOutNotMet { .. }, Some(f)) => Self::protection_failure(
                            &protection,
                            f,
                            i,
                            format!("leg {i} via {}: {e}", hop.provider),
                        ),
                        _ => SwapFailure {
                            code: "leg_failed".into(),
                            message: format!("leg {i} via {}: {e}", hop.provider),
                            leg_index: Some(i),
                        },
                    };
                    swap.legs[i].status = LegStatus::Failed;
                    swap.legs[i].error = Some(e.to_string());
                    self.event(
                        swap,
                        "leg.failed",
                        serde_json::json!({ "leg": i, "provider": hop.provider, "error": e }),
                    )
                    .await?;
                    return self.compensate(swap, fills, failure).await;
                }
            }
        }
        swap.status = SwapStatus::Settled;
        swap.amount_out = Some(running.clone());
        if let (Some(r), Some(h)) = (swap.reference_rate.clone(), hops.last()) {
            swap.price_impact_bps =
                Some(price_impact(&swap.amount_in, &running, &r, &h.to, &self.policy)?.impact_bps);
        }
        swap.updated_at = self.clock.now();
        self.store.update_swap(swap).await?;
        self.event(
            swap,
            "swap.settled",
            serde_json::json!({
                "amount_out": running,
                "price_impact_bps": swap.price_impact_bps,
            }),
        )
        .await?;
        Ok(())
    }

    /// Reverse every filled leg, newest first.
    async fn compensate(
        &self,
        swap: &mut SwapRecord,
        fills: Vec<(usize, LegFill)>,
        failure: SwapFailure,
    ) -> Result<(), EngineError> {
        for leg in swap.legs.iter_mut() {
            if leg.status == LegStatus::Pending {
                leg.status = LegStatus::Skipped;
            }
        }
        swap.status = SwapStatus::Compensating;
        swap.failure = Some(failure.clone());
        swap.updated_at = self.clock.now();
        self.store.update_swap(swap).await?;
        self.event(
            swap,
            "swap.compensating",
            serde_json::json!({ "code": failure.code, "message": failure.message, "legs_to_reverse": fills.len() }),
        )
        .await?;

        let mut all_ok = true;
        for (i, fill) in fills.into_iter().rev() {
            let result = match self.providers_by_id.get(&fill.provider) {
                Some(p) => p.compensate(&fill).await,
                None => Err(LiquidityError::Unavailable(
                    "provider not registered".into(),
                )),
            };
            match result {
                Ok(()) => {
                    swap.legs[i].status = LegStatus::Compensated;
                    self.event(
                        swap,
                        "leg.compensated",
                        serde_json::json!({ "leg": i, "reference": fill.reference }),
                    )
                    .await?;
                }
                Err(e) => {
                    all_ok = false;
                    swap.legs[i].status = LegStatus::CompensationFailed;
                    swap.legs[i].error = Some(e.to_string());
                    tracing::error!(swap_id = %swap.swap_id, leg = i, error = %e, "swap leg compensation failed");
                    self.event(
                        swap,
                        "leg.compensation_failed",
                        serde_json::json!({ "leg": i, "reference": fill.reference, "error": e }),
                    )
                    .await?;
                }
            }
        }
        swap.status = if all_ok {
            SwapStatus::Compensated
        } else {
            SwapStatus::CompensationFailed
        };
        swap.updated_at = self.clock.now();
        self.store.update_swap(swap).await?;
        let kind = if all_ok {
            "swap.compensated"
        } else {
            "swap.compensation_failed"
        };
        self.event(swap, kind, serde_json::json!({})).await?;
        Ok(())
    }

    async fn event(
        &self,
        swap: &SwapRecord,
        kind: &str,
        detail: serde_json::Value,
    ) -> Result<(), EngineError> {
        self.store
            .append_event(SwapEvent {
                swap_id: swap.swap_id.clone(),
                seq: 0,
                at: self.clock.now(),
                kind: kind.to_string(),
                detail,
            })
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swap::asset::Asset;
    use crate::swap::fees::FeeSchedule;
    use crate::swap::liquidity::{Pair, PairQuote, StaticRateProvider};
    use crate::swap::runtime::{ManualClock, SequentialIdGenerator};
    use async_trait::async_trait;
    use std::sync::Mutex;

    /// Static provider that can fail, or fill worse than it quoted.
    struct Flaky {
        inner: StaticRateProvider,
        fail_execute: bool,
        /// Fill this many bps below the quoted output.
        drift_bps: u32,
        /// Reject fills below `min_out` (a well-behaved provider).
        honours_min_out: bool,
        compensated: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl LiquidityProvider for Flaky {
        fn id(&self) -> &str {
            self.inner.id()
        }
        async fn pairs(&self) -> Result<Vec<Pair>, LiquidityError> {
            self.inner.pairs().await
        }
        async fn quote(
            &self,
            f: &Asset,
            t: &Asset,
            a: &Decimal,
        ) -> Result<PairQuote, LiquidityError> {
            self.inner.quote(f, t, a).await
        }
        async fn execute(&self, o: &LegOrder) -> Result<LegFill, LiquidityError> {
            if self.fail_execute {
                return Err(LiquidityError::Unavailable("down".into()));
            }
            let mut unchecked = o.clone();
            unchecked.min_out = None;
            let mut fill = self.inner.execute(&unchecked).await?;
            fill.amount_out = fill.amount_out.mul_bps(
                10_000 - self.drift_bps,
                o.to.decimals,
                crate::swap::rounding::RoundingMode::Down,
            );
            if let (true, Some(min)) = (self.honours_min_out, &o.min_out) {
                if &fill.amount_out < min {
                    return Err(LiquidityError::MinOutNotMet {
                        actual: fill.amount_out,
                        min: min.clone(),
                    });
                }
            }
            Ok(fill)
        }
        async fn compensate(&self, fill: &LegFill) -> Result<(), LiquidityError> {
            self.compensated
                .lock()
                .unwrap()
                .push(fill.reference.clone());
            Ok(())
        }
    }

    fn start() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[derive(Default)]
    struct Opts {
        fail_b: bool,
        drift_a: u32,
        drift_b: u32,
        ignore_min_out: bool,
        /// USDC->EURC reference rate.
        oracle: Option<&'static str>,
    }

    fn service(second_leg_fails: bool) -> (SwapService, Arc<Flaky>, Arc<ManualClock>) {
        service_with(Opts {
            fail_b: second_leg_fails,
            ..Opts::default()
        })
    }

    fn service_with(o: Opts) -> (SwapService, Arc<Flaky>, Arc<ManualClock>) {
        let a = Arc::new(Flaky {
            inner: StaticRateProvider::new("lp-a", FeeSchedule::bps(0))
                .with_rate("USDC", "XLM", "10"),
            fail_execute: false,
            drift_bps: o.drift_a,
            honours_min_out: !o.ignore_min_out,
            compensated: Mutex::default(),
        });
        let b = Arc::new(Flaky {
            inner: StaticRateProvider::new("lp-b", FeeSchedule::bps(0))
                .with_rate("XLM", "EURC", "0.09"),
            fail_execute: o.fail_b,
            drift_bps: o.drift_b,
            honours_min_out: !o.ignore_min_out,
            compensated: Mutex::default(),
        });
        let clock = Arc::new(ManualClock::new(start()));
        let mut builder = SwapService::builder(AssetRegistry::new(
            ["USDC", "XLM", "EURC"].map(Asset::stellar),
        ))
        .provider(a.clone())
        .provider(b)
        .clock(clock.clone())
        .ids(Arc::new(SequentialIdGenerator::default()));
        if let Some(rate) = o.oracle {
            builder = builder.oracle(Arc::new(
                crate::swap::oracle::StaticPriceOracle::new().with_rate("USDC", "EURC", rate),
            ));
        }
        (builder.build(), a, clock)
    }

    fn protect(min_received: Option<&str>, max_slippage_bps: Option<u32>) -> ProtectionParams {
        ProtectionParams {
            min_received: min_received.map(|m| Decimal::parse(m).unwrap()),
            max_slippage_bps,
        }
    }

    fn preq(amount: &str, p: ProtectionParams) -> QuoteRequest {
        QuoteRequest {
            protection: p,
            ..req(amount)
        }
    }

    fn req(amount: &str) -> QuoteRequest {
        QuoteRequest {
            source_asset: "USDC".into(),
            destination_asset: "EURC".into(),
            amount_in: Decimal::parse(amount).unwrap(),
            protection: ProtectionParams::default(),
        }
    }

    #[tokio::test]
    async fn multi_hop_quote_carries_route_into_execution() {
        let (svc, _, _) = service(false);
        let q = svc.quote(req("100")).await.unwrap();
        assert_eq!(q.quote_id, "quote_000001");
        assert_eq!(q.route.key(), "USDC>XLM@lp-a|XLM>EURC@lp-b");
        assert_eq!(q.expected_out.to_string(), "90.0000000");
        assert_eq!(q.rate.to_string(), "0.900000000000000000");

        let s = svc
            .execute(ExecuteRequest::new(q.quote_id.clone()))
            .await
            .unwrap();
        assert_eq!(s.status, SwapStatus::Settled);
        assert_eq!(s.route, q.route);
        assert_eq!(s.amount_out.unwrap().to_string(), "90.0000000");
        assert!(s.legs.iter().all(|l| l.status == LegStatus::Filled));
        let kinds: Vec<String> = svc
            .events(&s.swap_id)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(
            kinds,
            ["swap.executing", "leg.filled", "leg.filled", "swap.settled"]
        );
    }

    #[tokio::test]
    async fn failed_leg_compensates_earlier_legs() {
        let (svc, a, _) = service(true);
        let q = svc.quote(req("100")).await.unwrap();
        let s = svc.execute(ExecuteRequest::new(q.quote_id)).await.unwrap();
        assert_eq!(s.status, SwapStatus::Compensated);
        assert_eq!(s.legs[0].status, LegStatus::Compensated);
        assert_eq!(s.legs[1].status, LegStatus::Failed);
        assert_eq!(s.failure.as_ref().unwrap().code, "leg_failed");
        assert_eq!(a.compensated.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn execute_is_idempotent_per_quote() {
        let (svc, _, _) = service(false);
        let q = svc.quote(req("1")).await.unwrap();
        let r = ExecuteRequest::new(q.quote_id);
        let first = svc.execute(r.clone()).await.unwrap();
        let second = svc.execute(r).await.unwrap();
        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn expired_quote_is_rejected() {
        let (svc, _, clock) = service(false);
        let q = svc.quote(req("1")).await.unwrap();
        clock.advance(Duration::seconds(DEFAULT_QUOTE_TTL_SECS));
        let err = svc
            .execute(ExecuteRequest::new(q.quote_id))
            .await
            .unwrap_err();
        assert_eq!(err.code(), "quote_expired");
        assert_eq!(err.http_status(), 410);
    }

    #[tokio::test]
    async fn over_precise_amount_is_rejected_not_rounded() {
        let (svc, _, _) = service(false);
        let err = svc.quote(req("1.00000001")).await.unwrap_err();
        assert_eq!(err.code(), "invalid_amount");
    }

    #[tokio::test]
    async fn quote_refused_when_min_received_is_unattainable() {
        let (svc, _, _) = service(false);
        let err = svc
            .quote(preq("100", protect(Some("90.0000001"), None)))
            .await
            .unwrap_err();
        assert_eq!(err.code(), "min_received_unattainable");
        assert_eq!(err.http_status(), 422);
        let q = svc
            .quote(preq("100", protect(Some("90"), None)))
            .await
            .unwrap();
        assert_eq!(q.min_out.unwrap().to_string(), "90.0000000");
    }

    #[tokio::test]
    async fn quote_reports_price_impact_and_refuses_beyond_tolerance() {
        // Reference 0.95 EURC/USDC; route delivers 0.90 -> ~526 bps impact.
        let (svc, _, _) = service_with(Opts {
            oracle: Some("0.95"),
            ..Opts::default()
        });
        let err = svc
            .quote(preq("100", protect(None, Some(100))))
            .await
            .unwrap_err();
        assert_eq!(err.code(), "price_impact_exceeded");
        let q = svc
            .quote(preq("100", protect(None, Some(600))))
            .await
            .unwrap();
        assert_eq!(q.price_impact_bps.unwrap().to_string(), "526.32");
        assert_eq!(q.min_out.unwrap().to_string(), "89.3000000");
        // Without protection the impact is still reported.
        let q = svc.quote(req("100")).await.unwrap();
        assert!(q.price_impact_bps.is_some() && q.min_out.is_none());
    }

    #[tokio::test]
    async fn slippage_protection_fails_closed_without_an_oracle() {
        let (svc, _, _) = service(false);
        let err = svc
            .quote(preq("100", protect(None, Some(50))))
            .await
            .unwrap_err();
        assert_eq!(err.code(), "reference_price_unavailable");
        assert_eq!(err.http_status(), 503);
    }

    #[tokio::test]
    async fn invalid_protection_is_a_400() {
        let (svc, _, _) = service(false);
        let err = svc
            .quote(preq("100", protect(None, Some(9_000))))
            .await
            .unwrap_err();
        assert_eq!(err.code(), "invalid_protection");
        assert_eq!(err.http_status(), 400);
    }

    #[tokio::test]
    async fn final_leg_below_floor_is_aborted_and_compensated() {
        let (svc, a, _) = service_with(Opts {
            drift_b: 100, // final leg fills 1% short
            oracle: Some("0.90"),
            ..Opts::default()
        });
        let q = svc
            .quote(preq("100", protect(None, Some(50))))
            .await
            .unwrap();
        let s = svc.execute(ExecuteRequest::new(q.quote_id)).await.unwrap();
        assert_eq!(s.status, SwapStatus::Compensated);
        let failure = s.failure.unwrap();
        assert_eq!(failure.code, "slippage_exceeded");
        assert_eq!(failure.leg_index, Some(1));
        assert_eq!(s.legs[0].status, LegStatus::Compensated);
        assert_eq!(a.compensated.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn intermediate_drift_short_circuits_before_next_leg() {
        // Provider ignores min_out; the engine's own projection must stop it.
        let (svc, _, _) = service_with(Opts {
            drift_a: 200,
            ignore_min_out: true,
            ..Opts::default()
        });
        let q = svc
            .quote(preq("100", protect(Some("89.5"), None)))
            .await
            .unwrap();
        let s = svc.execute(ExecuteRequest::new(q.quote_id)).await.unwrap();
        assert_eq!(s.status, SwapStatus::Compensated);
        assert_eq!(s.failure.as_ref().unwrap().code, "min_received_not_met");
        assert_eq!(s.legs[0].status, LegStatus::Compensated);
        assert_eq!(s.legs[1].status, LegStatus::Skipped);
        let kinds: Vec<String> = svc
            .events(&s.swap_id)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(
            kinds,
            [
                "swap.executing",
                "leg.filled",
                "protection.tripped",
                "swap.compensating",
                "leg.compensated",
                "swap.compensated"
            ]
        );
    }

    #[tokio::test]
    async fn drift_within_tolerance_settles_with_realized_impact() {
        let (svc, _, _) = service_with(Opts {
            drift_b: 20,
            oracle: Some("0.90"),
            ..Opts::default()
        });
        let q = svc
            .quote(preq("100", protect(None, Some(50))))
            .await
            .unwrap();
        let s = svc.execute(ExecuteRequest::new(q.quote_id)).await.unwrap();
        assert_eq!(s.status, SwapStatus::Settled);
        assert_eq!(s.amount_out.unwrap().to_string(), "89.8200000");
        assert_eq!(s.price_impact_bps.unwrap().to_string(), "20.00");
        assert_eq!(s.min_out.unwrap().to_string(), "89.5500000");
    }

    #[tokio::test]
    async fn order_can_tighten_protection_and_short_circuits_before_any_leg() {
        let (svc, a, _) = service(false);
        let q = svc.quote(req("100")).await.unwrap();
        let err = svc
            .execute(
                ExecuteRequest::new(q.quote_id.clone()).with_protection(protect(Some("95"), None)),
            )
            .await
            .unwrap_err();
        assert_eq!(err.code(), "min_received_unattainable");
        assert!(svc
            .store
            .get_swap_by_quote(&q.quote_id)
            .await
            .unwrap()
            .is_none());
        assert!(a.compensated.lock().unwrap().is_empty());
        // A looser order-time minimum cannot loosen the quote's.
        let q = svc
            .quote(preq("100", protect(Some("90"), None)))
            .await
            .unwrap();
        let s = svc
            .execute(ExecuteRequest::new(q.quote_id).with_protection(protect(Some("1"), None)))
            .await
            .unwrap();
        assert_eq!(s.min_out.unwrap().to_string(), "90.0000000");
    }
}
