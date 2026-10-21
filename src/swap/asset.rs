//! Swap assets and their documented precision.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::swap::decimal::{Decimal, DecimalError, MAX_SCALE};

/// Fractional digits of a Stellar asset amount.
pub const STELLAR_DECIMALS: u32 = 7;

/// An asset the engine can quote, with the precision its amounts carry.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Asset {
    pub code: String,
    /// Fractional digits of an amount of this asset.
    pub decimals: u32,
}

impl Asset {
    pub fn new(code: impl Into<String>, decimals: u32) -> Self {
        assert!(decimals <= MAX_SCALE, "asset decimals exceed MAX_SCALE");
        Self {
            code: code.into(),
            decimals,
        }
    }

    /// A Stellar asset (7 decimals).
    pub fn stellar(code: impl Into<String>) -> Self {
        Self::new(code, STELLAR_DECIMALS)
    }

    /// Parse a client-supplied amount of this asset. More fractional digits
    /// than the asset supports is an error, never silently rounded.
    pub fn parse_amount(&self, s: &str) -> Result<Decimal, DecimalError> {
        Decimal::parse_with_scale(s, self.decimals)
    }

    /// Normalise an already-parsed amount to this asset's precision, failing
    /// if that would lose digits.
    pub fn normalize(&self, amount: &Decimal) -> Result<Decimal, DecimalError> {
        Decimal::parse_with_scale(&amount.to_string(), self.decimals)
    }

    pub fn zero(&self) -> Decimal {
        Decimal::zero(self.decimals)
    }
}

/// Known assets by code. Ordered so iteration is deterministic.
#[derive(Debug, Clone, Default)]
pub struct AssetRegistry {
    assets: BTreeMap<String, Asset>,
}

impl AssetRegistry {
    pub fn new(assets: impl IntoIterator<Item = Asset>) -> Self {
        Self {
            assets: assets.into_iter().map(|a| (a.code.clone(), a)).collect(),
        }
    }

    pub fn get(&self, code: &str) -> Option<&Asset> {
        self.assets.get(code)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Asset> {
        self.assets.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_amount_enforces_asset_precision() {
        let usdc = Asset::stellar("USDC");
        assert_eq!(usdc.parse_amount("10").unwrap().to_string(), "10.0000000");
        assert!(usdc.parse_amount("0.00000001").is_err());
        let btc = Asset::new("BTC", 8);
        assert_eq!(
            btc.parse_amount("0.00000001").unwrap().to_string(),
            "0.00000001"
        );
    }
}
