//! Reference-price oracles.
//!
//! An oracle supplies the *fair* exchange rate between two assets,
//! independent of any liquidity provider's quote. The engine measures price
//! impact and slippage tolerance against it (see
//! [`protection`](crate::swap::protection)).

use async_trait::async_trait;
use std::collections::BTreeMap;

use crate::swap::asset::Asset;
use crate::swap::decimal::{Decimal, RATE_SCALE};
use crate::swap::liquidity::Pair;
use crate::swap::rounding::{Purpose, RoundingPolicy};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OracleError {
    #[error("no reference price for {0}->{1}")]
    UnknownPair(String, String),
    #[error("price oracle unavailable: {0}")]
    Unavailable(String),
}

#[async_trait]
pub trait PriceOracle: Send + Sync {
    fn id(&self) -> &str;

    /// Units of `to` per unit of `from`, at [`RATE_SCALE`].
    async fn reference_rate(&self, from: &Asset, to: &Asset) -> Result<Decimal, OracleError>;
}

/// Fixed reference rates. A pair's inverse is derived when only one
/// direction is configured.
#[derive(Debug, Clone, Default)]
pub struct StaticPriceOracle {
    rates: BTreeMap<Pair, Decimal>,
    policy: RoundingPolicy,
}

impl StaticPriceOracle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_rate(mut self, from: &str, to: &str, rate: &str) -> Self {
        self.set_rate(
            from,
            to,
            Decimal::parse(rate).expect("oracle rate must be a decimal string"),
        );
        self
    }

    pub fn set_rate(&mut self, from: &str, to: &str, rate: Decimal) {
        self.rates.insert(
            Pair::new(from, to),
            rate.rescale(RATE_SCALE, self.policy.mode(Purpose::Rate)),
        );
    }

    pub fn rate(&self, from: &str, to: &str) -> Option<Decimal> {
        if let Some(r) = self.rates.get(&Pair::new(from, to)) {
            return Some(r.clone());
        }
        let inverse = self.rates.get(&Pair::new(to, from))?;
        Decimal::from_int(1)
            .div(inverse, RATE_SCALE, self.policy.mode(Purpose::Rate))
            .ok()
    }
}

#[async_trait]
impl PriceOracle for StaticPriceOracle {
    fn id(&self) -> &str {
        "static"
    }

    async fn reference_rate(&self, from: &Asset, to: &Asset) -> Result<Decimal, OracleError> {
        self.rate(&from.code, &to.code)
            .ok_or_else(|| OracleError::UnknownPair(from.code.clone(), to.code.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn static_oracle_derives_inverse() {
        let o = StaticPriceOracle::new().with_rate("USDC", "XLM", "8");
        let usdc = Asset::stellar("USDC");
        let xlm = Asset::stellar("XLM");
        assert_eq!(
            o.reference_rate(&xlm, &usdc).await.unwrap().to_string(),
            "0.125000000000000000"
        );
        assert!(o
            .reference_rate(&usdc, &Asset::stellar("BTC"))
            .await
            .is_err());
    }
}
