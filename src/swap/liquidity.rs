//! Liquidity-provider interface the Swap Engine routes across.
//!
//! A provider exposes the directed pairs it can trade, prices a single leg,
//! executes a leg, and (for compensation) reverses a leg it filled. All
//! amounts are [`Decimal`]s in the relevant asset's precision.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::swap::asset::Asset;
use crate::swap::decimal::{Decimal, RATE_SCALE};
use crate::swap::leg_fees::FeeSchedule;
use crate::swap::rate_conversion::{convert, implied_rate};
use crate::swap::rounding::{Purpose, RoundingPolicy};

/// A directed tradable pair: sell `from`, receive `to`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Pair {
    pub from: String,
    pub to: String,
}

impl Pair {
    pub fn new(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
        }
    }
}

/// A provider's price for one leg.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairQuote {
    /// Amount sold, in `from` precision.
    pub amount_in: Decimal,
    /// Amount received after fees, in `to` precision.
    pub amount_out: Decimal,
    /// Fee charged, in `from` precision.
    pub fee: Decimal,
}

/// Instruction to execute one leg of a route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegOrder {
    pub swap_id: String,
    pub leg_index: usize,
    pub from: Asset,
    pub to: Asset,
    pub amount_in: Decimal,
    /// The provider must not fill for less than this (if set).
    pub min_out: Option<Decimal>,
}

/// Result of executing one leg.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegFill {
    pub provider: String,
    /// Provider-side identifier for the fill (used to reverse it).
    pub reference: String,
    pub amount_in: Decimal,
    pub amount_out: Decimal,
    pub fee: Decimal,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum LiquidityError {
    #[error("pair {0}->{1} not supported")]
    UnsupportedPair(String, String),
    #[error("insufficient liquidity")]
    InsufficientLiquidity,
    #[error("fill below minimum: would receive {actual}, minimum {min}")]
    MinOutNotMet { actual: Decimal, min: Decimal },
    #[error("provider rejected the order: {0}")]
    Rejected(String),
    #[error("provider unavailable: {0}")]
    Unavailable(String),
}

#[async_trait]
pub trait LiquidityProvider: Send + Sync {
    /// Stable identifier, used in routes and tie-breaking.
    fn id(&self) -> &str;

    /// Directed pairs this provider can currently trade.
    async fn pairs(&self) -> Result<Vec<Pair>, LiquidityError>;

    /// Price selling `amount_in` of `from` for `to`.
    async fn quote(
        &self,
        from: &Asset,
        to: &Asset,
        amount_in: &Decimal,
    ) -> Result<PairQuote, LiquidityError>;

    /// Execute one leg.
    async fn execute(&self, order: &LegOrder) -> Result<LegFill, LiquidityError>;

    /// Reverse a fill this provider made (compensation after a later leg
    /// failed). Must be idempotent on `fill.reference`.
    async fn compensate(&self, fill: &LegFill) -> Result<(), LiquidityError>;
}

/// A provider with fixed rates and a fee schedule. Useful as a building
/// block for adapters with off-chain price feeds, and for tests.
#[derive(Debug, Clone)]
pub struct StaticRateProvider {
    id: String,
    rates: BTreeMap<Pair, Decimal>,
    fee: FeeSchedule,
    policy: RoundingPolicy,
}

impl StaticRateProvider {
    pub fn new(id: impl Into<String>, fee: FeeSchedule) -> Self {
        Self {
            id: id.into(),
            rates: BTreeMap::new(),
            fee,
            policy: RoundingPolicy::STANDARD,
        }
    }

    /// Add a directed pair at `rate` (`to` units per `from` unit).
    pub fn with_rate(mut self, from: &str, to: &str, rate: &str) -> Self {
        let rate = Decimal::parse(rate)
            .expect("static rate must be a decimal string")
            .rescale(RATE_SCALE, self.policy.mode(Purpose::Rate));
        self.rates.insert(Pair::new(from, to), rate);
        self
    }

    pub fn rate(&self, from: &str, to: &str) -> Option<&Decimal> {
        self.rates.get(&Pair::new(from, to))
    }

    /// Price a leg with the given purpose for the output rounding.
    pub fn price(
        &self,
        from: &Asset,
        to: &Asset,
        amount_in: &Decimal,
        purpose: Purpose,
    ) -> Result<PairQuote, LiquidityError> {
        let rate = self
            .rate(&from.code, &to.code)
            .ok_or_else(|| LiquidityError::UnsupportedPair(from.code.clone(), to.code.clone()))?;
        let (fee, net) = self.fee.split(amount_in, &self.policy);
        let amount_out = convert(&net, rate, to, purpose, &self.policy);
        Ok(PairQuote {
            amount_in: amount_in.clone(),
            amount_out,
            fee,
        })
    }

    pub fn policy(&self) -> &RoundingPolicy {
        &self.policy
    }
}

#[async_trait]
impl LiquidityProvider for StaticRateProvider {
    fn id(&self) -> &str {
        &self.id
    }

    async fn pairs(&self) -> Result<Vec<Pair>, LiquidityError> {
        Ok(self.rates.keys().cloned().collect())
    }

    async fn quote(
        &self,
        from: &Asset,
        to: &Asset,
        amount_in: &Decimal,
    ) -> Result<PairQuote, LiquidityError> {
        self.price(from, to, amount_in, Purpose::QuoteOutput)
    }

    async fn execute(&self, order: &LegOrder) -> Result<LegFill, LiquidityError> {
        let q = self.price(&order.from, &order.to, &order.amount_in, Purpose::Fill)?;
        if let Some(min) = &order.min_out {
            if &q.amount_out < min {
                return Err(LiquidityError::MinOutNotMet {
                    actual: q.amount_out,
                    min: min.clone(),
                });
            }
        }
        Ok(LegFill {
            provider: self.id.clone(),
            reference: format!("{}:{}:{}", self.id, order.swap_id, order.leg_index),
            amount_in: q.amount_in,
            amount_out: q.amount_out,
            fee: q.fee,
        })
    }

    async fn compensate(&self, _fill: &LegFill) -> Result<(), LiquidityError> {
        Ok(())
    }
}

/// Rate of a leg quote (`amount_out / amount_in`), for display.
pub fn quote_rate(q: &PairQuote, policy: &RoundingPolicy) -> Option<Decimal> {
    implied_rate(&q.amount_in, &q.amount_out, policy).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn static_provider_prices_with_fee_and_rounds_down() {
        let p = StaticRateProvider::new("lp", FeeSchedule::bps(30)).with_rate("USDC", "XLM", "8.5");
        let usdc = Asset::stellar("USDC");
        let xlm = Asset::stellar("XLM");
        let q = p
            .quote(&usdc, &xlm, &usdc.parse_amount("100").unwrap())
            .await
            .unwrap();
        assert_eq!(q.fee.to_string(), "0.3000000");
        assert_eq!(q.amount_out.to_string(), "847.4500000");
        assert!(p.quote(&xlm, &usdc, &q.amount_out).await.is_err());
    }

    #[tokio::test]
    async fn static_provider_enforces_min_out() {
        let p = StaticRateProvider::new("lp", FeeSchedule::bps(0)).with_rate("A", "B", "2");
        let order = LegOrder {
            swap_id: "s".into(),
            leg_index: 0,
            from: Asset::stellar("A"),
            to: Asset::stellar("B"),
            amount_in: Decimal::parse("1.0000000").unwrap(),
            min_out: Some(Decimal::parse("2.0000001").unwrap()),
        };
        assert!(matches!(
            p.execute(&order).await,
            Err(LiquidityError::MinOutNotMet { .. })
        ));
    }
}
