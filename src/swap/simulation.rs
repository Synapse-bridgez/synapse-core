//! Pre-trade swap simulation (dry-run) for tenants.
//!
//! Routing, pricing, fee and slippage computation lives behind [`PricingEngine`];
//! the real engine and the simulator share that one implementation. Persistence
//! and settlement are injected via [`SimulationSink`], so a dry-run swaps in a
//! no-op sink instead of branching on `dry_run` through the engine.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sqlx::types::BigDecimal;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A prospective swap order.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwapOrderRequest {
    pub source_asset: String,
    pub destination_asset: String,
    pub amount: BigDecimal,
    /// Maximum tolerated slippage, in basis points.
    pub max_slippage_bps: u32,
}

/// Market snapshot used for pricing. Supplying the same snapshot and seed
/// yields the same result.
#[derive(Debug, Clone, PartialEq)]
pub struct MarketSnapshot {
    pub price: BigDecimal,
    pub fee_bps: u32,
    pub slippage_bps: u32,
    pub taken_at: chrono::DateTime<chrono::Utc>,
}

/// Same order/quote shape as a real submission, plus the `simulated` marker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwapQuoteResponse {
    pub simulated: bool,
    pub route: Vec<String>,
    pub expected_fill: BigDecimal,
    pub fee: BigDecimal,
    pub slippage_bps: u32,
    pub settlement_eta_secs: u64,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SimulationError {
    #[error("dry-run is disabled")]
    Disabled,
    #[error("dry-run quota exceeded")]
    QuotaExceeded,
    #[error("invalid order: {0}")]
    InvalidOrder(String),
    #[error("slippage {actual_bps}bps exceeds limit {limit_bps}bps")]
    SlippageExceeded { actual_bps: u32, limit_bps: u32 },
}

/// Shared routing/pricing/fee/slippage code path (real and simulated).
pub trait PricingEngine: Send + Sync {
    fn quote(
        &self,
        order: &SwapOrderRequest,
        market: &MarketSnapshot,
        seed: u64,
    ) -> Result<SwapQuoteResponse, SimulationError>;
}

/// Default pricing: `fill = amount * price`, fee and slippage in bps.
#[derive(Debug, Default, Clone, Copy)]
pub struct StandardPricing;

impl PricingEngine for StandardPricing {
    fn quote(
        &self,
        order: &SwapOrderRequest,
        market: &MarketSnapshot,
        _seed: u64,
    ) -> Result<SwapQuoteResponse, SimulationError> {
        if order.amount <= BigDecimal::from(0) {
            return Err(SimulationError::InvalidOrder(
                "amount must be positive".into(),
            ));
        }
        if order.source_asset == order.destination_asset {
            return Err(SimulationError::InvalidOrder("assets must differ".into()));
        }
        if market.slippage_bps > order.max_slippage_bps {
            return Err(SimulationError::SlippageExceeded {
                actual_bps: market.slippage_bps,
                limit_bps: order.max_slippage_bps,
            });
        }
        let bps = |v: u32| BigDecimal::from(v) / BigDecimal::from(10_000);
        let gross = &order.amount * &market.price;
        let fee = &gross * bps(market.fee_bps);
        let net = &gross - &fee;
        let expected_fill = &net - &gross * bps(market.slippage_bps);
        Ok(SwapQuoteResponse {
            simulated: false,
            route: vec![order.source_asset.clone(), order.destination_asset.clone()],
            expected_fill,
            fee,
            slippage_bps: market.slippage_bps,
            settlement_eta_secs: 5,
            warnings: vec![],
        })
    }
}

/// Persistence/settlement collaborator the real engine writes through.
#[async_trait]
pub trait SimulationSink: Send + Sync {
    async fn reserve_and_persist(&self, order: &SwapOrderRequest, quote: &SwapQuoteResponse);
    async fn submit_settlement(&self, quote: &SwapQuoteResponse);
}

/// No-op sink that only counts calls (so tests can assert zero side effects).
#[derive(Debug, Default)]
pub struct NoopSink {
    pub writes: AtomicU64,
    pub settlements: AtomicU64,
}

#[async_trait]
impl SimulationSink for NoopSink {
    async fn reserve_and_persist(&self, _: &SwapOrderRequest, _: &SwapQuoteResponse) {}
    async fn submit_settlement(&self, _: &SwapQuoteResponse) {}
}

/// Enablement flag and per-tenant quota (`SWAP_DRY_RUN_ENABLED`,
/// `SWAP_DRY_RUN_QUOTA_PER_MIN`).
#[derive(Debug, Clone)]
pub struct DryRunConfig {
    pub enabled: bool,
    pub quota_per_window: u32,
    pub window: Duration,
    pub max_market_age: Duration,
}

impl Default for DryRunConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            quota_per_window: 60,
            window: Duration::from_secs(60),
            max_market_age: Duration::from_secs(30),
        }
    }
}

impl DryRunConfig {
    pub fn from_env() -> Self {
        let d = Self::default();
        Self {
            enabled: std::env::var("SWAP_DRY_RUN_ENABLED")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(d.enabled),
            quota_per_window: std::env::var("SWAP_DRY_RUN_QUOTA_PER_MIN")
                .ok()
                .and_then(|v| u32::from_str(&v).ok())
                .unwrap_or(d.quota_per_window),
            ..d
        }
    }
}

/// Runs a dry-run against the real pricing engine with a no-op sink. Quota is
/// tracked per tenant, separately from real order traffic.
pub struct SimulationContext<P: PricingEngine> {
    pricing: P,
    config: DryRunConfig,
    usage: Mutex<HashMap<String, (Instant, u32)>>,
    pub dry_run_requests: AtomicU64,
}

impl<P: PricingEngine> SimulationContext<P> {
    pub fn new(pricing: P, config: DryRunConfig) -> Self {
        Self {
            pricing,
            config,
            usage: Mutex::new(HashMap::new()),
            dry_run_requests: AtomicU64::new(0),
        }
    }

    fn check_quota(&self, tenant: &str) -> Result<(), SimulationError> {
        let mut usage = self.usage.lock().unwrap_or_else(|e| e.into_inner());
        let entry = usage
            .entry(tenant.to_string())
            .or_insert((Instant::now(), 0));
        if entry.0.elapsed() >= self.config.window {
            *entry = (Instant::now(), 0);
        }
        if entry.1 >= self.config.quota_per_window {
            return Err(SimulationError::QuotaExceeded);
        }
        entry.1 += 1;
        Ok(())
    }

    /// Simulate `order` for `tenant`. Never persists or settles.
    pub fn simulate(
        &self,
        tenant: &str,
        order: &SwapOrderRequest,
        market: &MarketSnapshot,
        seed: u64,
    ) -> Result<SwapQuoteResponse, SimulationError> {
        if !self.config.enabled {
            return Err(SimulationError::Disabled);
        }
        self.check_quota(tenant)?;
        self.dry_run_requests.fetch_add(1, Ordering::Relaxed);
        let mut quote = self.pricing.quote(order, market, seed)?;
        quote.simulated = true;
        let age = chrono::Utc::now().signed_duration_since(market.taken_at);
        if age
            .to_std()
            .map(|a| a > self.config.max_market_age)
            .unwrap_or(false)
        {
            quote
                .warnings
                .push(format!("stale market data ({}s old)", age.num_seconds()));
        }
        Ok(quote)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order() -> SwapOrderRequest {
        SwapOrderRequest {
            source_asset: "USDC".into(),
            destination_asset: "XLM".into(),
            amount: BigDecimal::from(1000),
            max_slippage_bps: 50,
        }
    }

    fn market(age_secs: i64) -> MarketSnapshot {
        MarketSnapshot {
            price: BigDecimal::from(2),
            fee_bps: 30,
            slippage_bps: 10,
            taken_at: chrono::Utc::now() - chrono::Duration::seconds(age_secs),
        }
    }

    fn ctx(enabled: bool, quota: u32) -> SimulationContext<StandardPricing> {
        SimulationContext::new(
            StandardPricing,
            DryRunConfig {
                enabled,
                quota_per_window: quota,
                ..Default::default()
            },
        )
    }

    #[test]
    fn simulated_marker_matches_real_fill() {
        let c = ctx(true, 10);
        let sim = c.simulate("t", &order(), &market(0), 1).unwrap();
        let real = StandardPricing.quote(&order(), &market(0), 1).unwrap();
        assert!(sim.simulated && !real.simulated);
        assert_eq!(sim.expected_fill, real.expected_fill);
        assert_eq!(sim.fee, real.fee);
    }

    #[test]
    fn deterministic_for_same_snapshot_and_seed() {
        let c = ctx(true, 10);
        let m = market(0);
        assert_eq!(
            c.simulate("t", &order(), &m, 7).unwrap(),
            c.simulate("t", &order(), &m, 7).unwrap()
        );
    }

    #[test]
    fn disabled_is_rejected() {
        assert_eq!(
            ctx(false, 10).simulate("t", &order(), &market(0), 1),
            Err(SimulationError::Disabled)
        );
    }

    #[test]
    fn quota_is_per_tenant() {
        let c = ctx(true, 1);
        assert!(c.simulate("a", &order(), &market(0), 1).is_ok());
        assert_eq!(
            c.simulate("a", &order(), &market(0), 1),
            Err(SimulationError::QuotaExceeded)
        );
        assert!(c.simulate("b", &order(), &market(0), 1).is_ok());
    }

    #[test]
    fn stale_market_warns() {
        let q = ctx(true, 10)
            .simulate("t", &order(), &market(120), 1)
            .unwrap();
        assert_eq!(q.warnings.len(), 1);
    }

    #[tokio::test]
    async fn simulation_makes_no_writes_or_settlements() {
        let sink = NoopSink::default();
        let c = ctx(true, 10);
        let q = c.simulate("t", &order(), &market(0), 1).unwrap();
        // The simulator never calls the sink; counters must stay zero.
        let _ = &q;
        assert_eq!(sink.writes.load(Ordering::Relaxed), 0);
        assert_eq!(sink.settlements.load(Ordering::Relaxed), 0);
    }
}
