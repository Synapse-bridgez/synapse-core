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
//!
//! Every state change is persisted to the [`SwapStore`] and appended to the
//! swap's event log.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::swap::asset::AssetRegistry;
use crate::swap::decimal::{Decimal, DecimalError};
use crate::swap::liquidity::{LegFill, LegOrder, LiquidityProvider};
use crate::swap::pricing::implied_rate;
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
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecuteRequest {
    pub quote_id: String,
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
            | EngineError::Routing(RoutingError::NonPositiveAmount) => 400,
            EngineError::Routing(_) => 422,
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
            created_at: now,
            expires_at: now + self.quote_ttl,
        };
        self.store.insert_quote(&quote).await?;
        Ok(quote)
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

        let mut swap = SwapRecord {
            swap_id: self.ids.next_id("swap"),
            quote_id: quote.quote_id.clone(),
            status: SwapStatus::Executing,
            route: quote.route.clone(),
            amount_in: quote.amount_in.clone(),
            expected_out: quote.expected_out.clone(),
            amount_out: None,
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
            serde_json::json!({ "quote_id": swap.quote_id, "route": swap.route.key() }),
        )
        .await?;

        self.run_legs(&mut swap).await?;
        Ok(swap)
    }

    async fn run_legs(&self, swap: &mut SwapRecord) -> Result<(), EngineError> {
        let mut running = swap.amount_in.clone();
        let mut fills: Vec<(usize, LegFill)> = Vec::new();
        let hops = swap.route.hops.clone();
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
            let order = LegOrder {
                swap_id: swap.swap_id.clone(),
                leg_index: i,
                from: hop.from.clone(),
                to: hop.to.clone(),
                amount_in: running.clone(),
                min_out: None,
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
                }
                Err(e) => {
                    let failure = SwapFailure {
                        code: "leg_failed".into(),
                        message: format!("leg {i} via {}: {e}", hop.provider),
                        leg_index: Some(i),
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
        swap.updated_at = self.clock.now();
        self.store.update_swap(swap).await?;
        self.event(
            swap,
            "swap.settled",
            serde_json::json!({ "amount_out": running }),
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
                None => Err(crate::swap::liquidity::LiquidityError::Unavailable(
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
    use crate::swap::liquidity::{LiquidityError, Pair, PairQuote, StaticRateProvider};
    use crate::swap::runtime::{ManualClock, SequentialIdGenerator};
    use async_trait::async_trait;
    use std::sync::Mutex;

    /// Static provider that can be told to fail execution.
    struct Flaky {
        inner: StaticRateProvider,
        fail_execute: bool,
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
            self.inner.execute(o).await
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

    fn service(second_leg_fails: bool) -> (SwapService, Arc<Flaky>, Arc<ManualClock>) {
        let a = Arc::new(Flaky {
            inner: StaticRateProvider::new("lp-a", FeeSchedule::bps(0))
                .with_rate("USDC", "XLM", "10"),
            fail_execute: false,
            compensated: Mutex::default(),
        });
        let b = Arc::new(Flaky {
            inner: StaticRateProvider::new("lp-b", FeeSchedule::bps(0))
                .with_rate("XLM", "EURC", "0.09"),
            fail_execute: second_leg_fails,
            compensated: Mutex::default(),
        });
        let clock = Arc::new(ManualClock::new(start()));
        let svc = SwapService::builder(AssetRegistry::new(
            ["USDC", "XLM", "EURC"].map(Asset::stellar),
        ))
        .provider(a.clone())
        .provider(b)
        .clock(clock.clone())
        .ids(Arc::new(SequentialIdGenerator::default()))
        .build();
        (svc, a, clock)
    }

    fn req(amount: &str) -> QuoteRequest {
        QuoteRequest {
            source_asset: "USDC".into(),
            destination_asset: "EURC".into(),
            amount_in: Decimal::parse(amount).unwrap(),
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
            .execute(ExecuteRequest {
                quote_id: q.quote_id.clone(),
            })
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
        let s = svc
            .execute(ExecuteRequest {
                quote_id: q.quote_id,
            })
            .await
            .unwrap();
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
        let r = ExecuteRequest {
            quote_id: q.quote_id,
        };
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
            .execute(ExecuteRequest {
                quote_id: q.quote_id,
            })
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
}
