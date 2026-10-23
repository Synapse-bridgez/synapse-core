//! Client-specified swap protection.
//!
//! A client can attach [`ProtectionParams`] to a quote request and/or an
//! execution request:
//!
//! * `min_received` — absolute minimum amount of the destination asset.
//! * `max_slippage_bps` — maximum shortfall of the realized output versus
//!   the **reference rate** from the [`PriceOracle`](crate::swap::oracle),
//!   in basis points.
//!
//! Both collapse into one number, the **floor**:
//!
//! ```text
//! reference_out = amount_in × reference_rate
//! floor         = max(min_received, reference_out × (1 − max_slippage_bps / 10 000))
//! ```
//!
//! rounded *up* to the destination precision (never loosened by rounding).
//!
//! The engine enforces the floor at every stage:
//!
//! 1. **Quote** — if the route's expected output is already below the floor
//!    the quote is refused (`min_received_unattainable` /
//!    `price_impact_exceeded`); nothing is stored.
//! 2. **Execution start** — the reference rate is re-read and the floor
//!    recomputed with any tighter execution-time parameters; if the quoted
//!    output no longer clears it, execution is refused before any leg runs.
//! 3. **Each intermediate leg** — the leg is sent with a proportional
//!    `min_out`, and after it fills the engine projects the final output
//!    along the remaining quoted legs. If the projection falls below the
//!    floor it stops (short-circuits) and compensates the filled legs.
//! 4. **Final leg** — sent with `min_out = floor`; a realized output below
//!    the floor is compensated.
//!
//! Price impact is reported as signed bps (positive = worse than the
//! reference), rounded toward +∞ so the tolerance check fails safe.

use serde::{Deserialize, Serialize};

use crate::swap::asset::Asset;
use crate::swap::decimal::{Decimal, DecimalError, BPS_DENOMINATOR, BPS_SCALE};
use crate::swap::rounding::{Purpose, RoundingPolicy};

/// Largest tolerance a client may request (50%).
pub const MAX_SLIPPAGE_BPS: u32 = 5_000;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtectionParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_received: Option<Decimal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_slippage_bps: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtectionError {
    #[error("min_received must be positive")]
    NonPositiveMinimum,
    #[error("min_received: {0}")]
    MinimumPrecision(DecimalError),
    #[error("max_slippage_bps must be at most {MAX_SLIPPAGE_BPS}")]
    SlippageTooLarge,
}

impl ProtectionParams {
    pub fn is_empty(&self) -> bool {
        self.min_received.is_none() && self.max_slippage_bps.is_none()
    }

    /// Validate against the destination asset and normalise `min_received`
    /// to its precision (excess digits are an error, not rounded).
    pub fn validated(&self, destination: &Asset) -> Result<Self, ProtectionError> {
        let min_received = match &self.min_received {
            Some(m) => {
                let m = destination
                    .normalize(m)
                    .map_err(ProtectionError::MinimumPrecision)?;
                if !m.is_positive() {
                    return Err(ProtectionError::NonPositiveMinimum);
                }
                Some(m)
            }
            None => None,
        };
        if let Some(bps) = self.max_slippage_bps {
            if bps > MAX_SLIPPAGE_BPS {
                return Err(ProtectionError::SlippageTooLarge);
            }
        }
        Ok(Self {
            min_received,
            max_slippage_bps: self.max_slippage_bps,
        })
    }

    /// Combine with another set, keeping the stricter of each parameter.
    /// Execution-time parameters can tighten a quote's, never loosen it.
    pub fn tightened_by(&self, other: &Self) -> Self {
        let min_received = match (&self.min_received, &other.min_received) {
            (Some(a), Some(b)) => Some(a.clone().max(b.clone())),
            (a, b) => a.clone().or_else(|| b.clone()),
        };
        let max_slippage_bps = match (self.max_slippage_bps, other.max_slippage_bps) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        Self {
            min_received,
            max_slippage_bps,
        }
    }
}

/// Output at the reference rate, plus how far an actual output is from it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceImpact {
    pub reference_rate: Decimal,
    /// `amount_in × reference_rate` in destination precision (rounded down).
    pub reference_out: Decimal,
    /// `(reference_out_exact − out) / reference_out_exact × 10 000`, at
    /// [`BPS_SCALE`], rounded toward +∞. Positive = worse than reference.
    pub impact_bps: Decimal,
}

pub fn price_impact(
    amount_in: &Decimal,
    out: &Decimal,
    reference_rate: &Decimal,
    destination: &Asset,
    policy: &RoundingPolicy,
) -> Result<PriceImpact, DecimalError> {
    // Exact reference output; scale is the sum of both scales.
    let exact = amount_in.mul(
        reference_rate,
        amount_in.scale() + reference_rate.scale(),
        policy.mode(Purpose::Rate),
    );
    let shortfall = exact.sub(out);
    let impact_bps = shortfall
        .mul(
            &Decimal::from_int(BPS_DENOMINATOR as i64),
            shortfall.scale(),
            policy.mode(Purpose::PriceImpact),
        )
        .div(&exact, BPS_SCALE, policy.mode(Purpose::PriceImpact))?;
    Ok(PriceImpact {
        reference_rate: reference_rate.clone(),
        reference_out: exact.rescale(destination.decimals, policy.mode(Purpose::QuoteOutput)),
        impact_bps,
    })
}

/// The effective minimum output for selling `amount_in`, given the
/// reference rate (required when `max_slippage_bps` is set). `None` when
/// unprotected.
pub fn floor(
    params: &ProtectionParams,
    amount_in: &Decimal,
    reference_rate: Option<&Decimal>,
    destination: &Asset,
    policy: &RoundingPolicy,
) -> Option<Decimal> {
    let mode = policy.mode(Purpose::ProtectionFloor);
    let from_slippage = match (params.max_slippage_bps, reference_rate) {
        (Some(bps), Some(rate)) => {
            // Exact reference output, so the floor is rounded only once.
            let exact = amount_in.mul(rate, amount_in.scale() + rate.scale(), mode);
            Some(exact.mul_bps(BPS_DENOMINATOR - bps, destination.decimals, mode))
        }
        _ => None,
    };
    match (params.min_received.clone(), from_slippage) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

/// `floor × part / whole`, rounded up — the proportional minimum for an
/// intermediate leg whose quoted output is `part` when the whole route's is
/// `whole`.
pub fn proportional_floor(
    floor: &Decimal,
    part: &Decimal,
    whole: &Decimal,
    scale: u32,
    policy: &RoundingPolicy,
) -> Result<Decimal, DecimalError> {
    let mode = policy.mode(Purpose::ProtectionFloor);
    floor
        .mul(part, floor.scale() + part.scale(), mode)
        .div(whole, scale, mode)
}

/// Final output implied by `actual` at leg `k` if the remaining legs fill
/// at their quoted rates: `actual × route_out / leg_k_quoted_out`, rounded
/// down.
pub fn projected_output(
    actual: &Decimal,
    leg_quoted_out: &Decimal,
    route_expected_out: &Decimal,
    scale: u32,
    policy: &RoundingPolicy,
) -> Result<Decimal, DecimalError> {
    let mode = policy.mode(Purpose::QuoteOutput);
    actual
        .mul(
            route_expected_out,
            actual.scale() + route_expected_out.scale(),
            mode,
        )
        .div(leg_quoted_out, scale, mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        Decimal::parse(s).unwrap()
    }

    fn eurc() -> Asset {
        Asset::stellar("EURC")
    }

    #[test]
    fn validation_normalises_and_bounds() {
        let p = ProtectionParams {
            min_received: Some(d("1.5")),
            max_slippage_bps: Some(50),
        }
        .validated(&eurc())
        .unwrap();
        assert_eq!(p.min_received.unwrap().to_string(), "1.5000000");
        let bad = |p: ProtectionParams| p.validated(&eurc()).unwrap_err();
        assert_eq!(
            bad(ProtectionParams {
                min_received: Some(d("0")),
                ..Default::default()
            }),
            ProtectionError::NonPositiveMinimum
        );
        assert!(matches!(
            bad(ProtectionParams {
                min_received: Some(d("1.00000001")),
                ..Default::default()
            }),
            ProtectionError::MinimumPrecision(_)
        ));
        assert_eq!(
            bad(ProtectionParams {
                max_slippage_bps: Some(5_001),
                ..Default::default()
            }),
            ProtectionError::SlippageTooLarge
        );
    }

    #[test]
    fn tightening_keeps_the_stricter_value() {
        let quote = ProtectionParams {
            min_received: Some(d("10")),
            max_slippage_bps: Some(100),
        };
        let exec = ProtectionParams {
            min_received: Some(d("9")),
            max_slippage_bps: Some(30),
        };
        let t = quote.tightened_by(&exec);
        assert_eq!(t.min_received, Some(d("10")));
        assert_eq!(t.max_slippage_bps, Some(30));
        assert_eq!(ProtectionParams::default().tightened_by(&exec), exec);
    }

    #[test]
    fn price_impact_is_signed_and_rounds_toward_rejection() {
        let p = RoundingPolicy::STANDARD;
        // reference 100 * 0.9 = 90; actual 89.1 -> 100 bps worse
        let i = price_impact(&d("100.0000000"), &d("89.1000000"), &d("0.9"), &eurc(), &p).unwrap();
        assert_eq!(i.reference_out.to_string(), "90.0000000");
        assert_eq!(i.impact_bps.to_string(), "100.00");
        // better than reference -> negative
        let i = price_impact(&d("100"), &d("90.9"), &d("0.9"), &eurc(), &p).unwrap();
        assert_eq!(i.impact_bps.to_string(), "-100.00");
        // 1/3 bps worse rounds up to 0.34
        let i = price_impact(&d("3"), &d("2.9999"), &d("1"), &eurc(), &p).unwrap();
        assert_eq!(i.impact_bps.to_string(), "0.34");
    }

    #[test]
    fn floor_combines_minimum_and_slippage() {
        let p = RoundingPolicy::STANDARD;
        let r = d("0.9");
        let amt = d("100.0000000");
        let slip = ProtectionParams {
            max_slippage_bps: Some(50),
            ..Default::default()
        };
        assert_eq!(
            floor(&slip, &amt, Some(&r), &eurc(), &p)
                .unwrap()
                .to_string(),
            "89.5500000"
        );
        let both = ProtectionParams {
            min_received: Some(d("89.9")),
            max_slippage_bps: Some(50),
        };
        assert_eq!(
            floor(&both, &amt, Some(&r), &eurc(), &p).unwrap(),
            d("89.9")
        );
        assert_eq!(
            floor(&ProtectionParams::default(), &amt, Some(&r), &eurc(), &p),
            None
        );
        // slippage without a reference price cannot produce a floor
        assert_eq!(floor(&slip, &amt, None, &eurc(), &p), None);
    }

    #[test]
    fn proportional_floor_and_projection() {
        let p = RoundingPolicy::STANDARD;
        // route 1000 XLM -> 90 EURC; floor 89.55 -> leg-1 floor 995 XLM
        assert_eq!(
            proportional_floor(
                &d("89.5500000"),
                &d("1000.0000000"),
                &d("90.0000000"),
                7,
                &p
            )
            .unwrap()
            .to_string(),
            "995.0000000"
        );
        assert_eq!(
            projected_output(
                &d("990.0000000"),
                &d("1000.0000000"),
                &d("90.0000000"),
                7,
                &p
            )
            .unwrap()
            .to_string(),
            "89.1000000"
        );
    }
}
