//! Rate conversion helpers. Every place the engine turns an amount of one
//! asset into another, or derives a rate from two amounts, uses these.

use crate::swap::asset::Asset;
use crate::swap::decimal::{Decimal, DecimalError, RATE_SCALE};
use crate::swap::rounding::{Purpose, RoundingPolicy};

/// `amount * rate`, expressed in `to`'s precision, rounded for `purpose`.
pub fn convert(
    amount: &Decimal,
    rate: &Decimal,
    to: &Asset,
    purpose: Purpose,
    policy: &RoundingPolicy,
) -> Decimal {
    amount.mul(rate, to.decimals, policy.mode(purpose))
}

/// Rate implied by receiving `amount_out` for `amount_in`, at
/// [`RATE_SCALE`], rounded with the policy's [`Purpose::Rate`] mode.
pub fn implied_rate(
    amount_in: &Decimal,
    amount_out: &Decimal,
    policy: &RoundingPolicy,
) -> Result<Decimal, DecimalError> {
    amount_out.div(amount_in, RATE_SCALE, policy.mode(Purpose::Rate))
}

/// Normalise a rate to [`RATE_SCALE`].
pub fn normalize_rate(rate: &Decimal, policy: &RoundingPolicy) -> Decimal {
    rate.rescale(RATE_SCALE, policy.mode(Purpose::Rate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_rounds_down_for_fills_and_quotes() {
        let p = RoundingPolicy::STANDARD;
        let xlm = Asset::stellar("XLM");
        let amt = Decimal::parse("3.0000000").unwrap();
        let rate = Decimal::parse("0.333333333333333333").unwrap();
        assert_eq!(
            convert(&amt, &rate, &xlm, Purpose::Fill, &p).to_string(),
            "0.9999999"
        );
        assert_eq!(
            convert(&amt, &rate, &xlm, Purpose::QuoteInput, &p).to_string(),
            "1.0000000"
        );
    }

    #[test]
    fn implied_rate_has_rate_scale() {
        let p = RoundingPolicy::STANDARD;
        let r = implied_rate(
            &Decimal::parse("3").unwrap(),
            &Decimal::parse("1").unwrap(),
            &p,
        )
        .unwrap();
        assert_eq!(r.to_string(), "0.333333333333333333");
        assert!(implied_rate(&Decimal::zero(7), &Decimal::zero(7), &p).is_err());
    }
}
