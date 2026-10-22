//! Platform fee and spread computation for swap orders.
//!
//! All monetary values are integer base units of the order's input asset
//! (e.g. stroops for 7-decimal assets); rates are basis points (1 bps = 0.01%).
//! No floats are involved.
//!
//! Rounding rule: every component is rounded *down* (floor), and the rounding
//! remainder stays with the user's output. Therefore
//! `input_amount == platform_fee + spread + output_amount` holds exactly by
//! construction, and rounding never creates or destroys value.

use std::collections::HashMap;
use std::fmt;

/// Monetary amount in base units of the asset.
pub type Amount = i128;

/// Basis-point denominator (10_000 bps = 100%).
const BPS_DENOMINATOR: Amount = 10_000;

/// A fee and spread schedule.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FeeRule {
    /// Percentage component of the platform fee, in bps of the input amount.
    pub percentage_bps: u32,
    /// Flat component of the platform fee, in base units of the input asset.
    pub flat: Amount,
    /// Spread, in bps of the post-fee amount.
    pub spread_bps: u32,
}

/// Fee configuration with per-tenant and per-asset-pair overrides.
#[derive(Debug, Clone, Default)]
pub struct FeeConfig {
    /// Schedule used when no override matches.
    pub default: FeeRule,
    /// Overrides keyed by tenant id.
    pub tenant_overrides: HashMap<String, FeeRule>,
    /// Overrides keyed by asset pair (e.g. `"USDC/XLM"`).
    pub pair_overrides: HashMap<String, FeeRule>,
}

impl FeeConfig {
    /// Resolves the rule for a tenant and asset pair.
    ///
    /// Precedence: tenant override, then asset-pair override, then default.
    pub fn rule_for(&self, tenant_id: &str, asset_pair: &str) -> FeeRule {
        self.tenant_overrides
            .get(tenant_id)
            .or_else(|| self.pair_overrides.get(asset_pair))
            .copied()
            .unwrap_or(self.default)
    }
}

/// Input to [`compute_fees`].
#[derive(Debug, Clone, Copy)]
pub struct FeeRequest<'a> {
    pub tenant_id: &'a str,
    pub asset_pair: &'a str,
    pub input_amount: Amount,
}

/// Itemized result of [`compute_fees`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeBreakdown {
    pub input_amount: Amount,
    pub percentage_fee: Amount,
    pub flat_fee: Amount,
    pub spread: Amount,
    pub output_amount: Amount,
}

impl FeeBreakdown {
    /// Percentage plus flat platform fee.
    pub fn platform_fee(&self) -> Amount {
        self.percentage_fee + self.flat_fee
    }

    /// Platform fee plus spread.
    pub fn total_fees(&self) -> Amount {
        self.platform_fee() + self.spread
    }
}

/// Errors returned by [`compute_fees`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeeError {
    /// The input amount is zero or negative.
    NonPositiveAmount,
    /// Fees and spread consume the entire input amount.
    FeeExceedsInput,
    /// The fee rule is out of range (bps above 10_000 or negative flat fee).
    InvalidRule,
    /// Intermediate arithmetic overflowed.
    Overflow,
}

impl fmt::Display for FeeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FeeError::NonPositiveAmount => write!(f, "input amount must be positive"),
            FeeError::FeeExceedsInput => write!(f, "fees exceed input amount"),
            FeeError::InvalidRule => write!(f, "invalid fee rule"),
            FeeError::Overflow => write!(f, "fee arithmetic overflow"),
        }
    }
}

impl std::error::Error for FeeError {}

/// `amount * bps / 10_000`, rounded down.
fn bps_of(amount: Amount, bps: u32) -> Result<Amount, FeeError> {
    amount
        .checked_mul(Amount::from(bps))
        .map(|scaled| scaled / BPS_DENOMINATOR)
        .ok_or(FeeError::Overflow)
}

/// Computes the itemized fee breakdown for a swap order.
///
/// Pure and deterministic: the same request and config always yield the same
/// breakdown, so a quote can be recomputed at execution time.
pub fn compute_fees(
    request: &FeeRequest<'_>,
    config: &FeeConfig,
) -> Result<FeeBreakdown, FeeError> {
    let input = request.input_amount;
    if input <= 0 {
        return Err(FeeError::NonPositiveAmount);
    }

    let rule = config.rule_for(request.tenant_id, request.asset_pair);
    if rule.percentage_bps > BPS_DENOMINATOR as u32
        || rule.spread_bps > BPS_DENOMINATOR as u32
        || rule.flat < 0
    {
        return Err(FeeError::InvalidRule);
    }

    let percentage_fee = bps_of(input, rule.percentage_bps)?;
    let flat_fee = rule.flat;
    let after_fee = input
        .checked_sub(percentage_fee)
        .and_then(|v| v.checked_sub(flat_fee))
        .ok_or(FeeError::Overflow)?;
    if after_fee <= 0 {
        return Err(FeeError::FeeExceedsInput);
    }

    let spread = bps_of(after_fee, rule.spread_bps)?;
    let output_amount = after_fee - spread;
    if output_amount <= 0 {
        return Err(FeeError::FeeExceedsInput);
    }

    Ok(FeeBreakdown {
        input_amount: input,
        percentage_fee,
        flat_fee,
        spread,
        output_amount,
    })
}
