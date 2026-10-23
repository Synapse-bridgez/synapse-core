//! Fee computation for swap legs.

use serde::{Deserialize, Serialize};

use crate::swap::decimal::{Decimal, BPS_DENOMINATOR};
use crate::swap::rounding::{Purpose, RoundingPolicy};

/// A fee of `bps` basis points of the amount plus an optional flat amount,
/// both charged in the asset being paid in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeeSchedule {
    pub bps: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flat: Option<Decimal>,
}

impl FeeSchedule {
    pub fn bps(bps: u32) -> Self {
        assert!(bps <= BPS_DENOMINATOR, "fee bps above 100%");
        Self { bps, flat: None }
    }

    pub fn with_flat(mut self, flat: Decimal) -> Self {
        self.flat = Some(flat);
        self
    }

    /// Fee on `amount`, at `amount`'s scale, rounded with the policy's
    /// [`Purpose::Fee`] mode and capped at `amount` (a fee never makes the
    /// net amount negative).
    pub fn fee_for(&self, amount: &Decimal, policy: &RoundingPolicy) -> Decimal {
        let scale = amount.scale();
        let mode = policy.mode(Purpose::Fee);
        let mut fee = amount.mul_bps(self.bps, scale, mode);
        if let Some(flat) = &self.flat {
            fee = fee.add(&flat.rescale(scale, mode));
        }
        fee.min(amount.clone()).rescale(scale, mode)
    }

    /// `(fee, amount - fee)`.
    pub fn split(&self, amount: &Decimal, policy: &RoundingPolicy) -> (Decimal, Decimal) {
        let fee = self.fee_for(amount, policy);
        let net = amount.sub(&fee);
        (fee, net)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        Decimal::parse(s).unwrap()
    }

    #[test]
    fn fee_rounds_up_and_net_plus_fee_is_exact() {
        let p = RoundingPolicy::STANDARD;
        let (fee, net) = FeeSchedule::bps(30).split(&d("1.0000001"), &p);
        // exact fee = 0.003000003 -> rounded up to 0.0030001
        assert_eq!(fee.to_string(), "0.0030001");
        assert_eq!(net.to_string(), "0.9970000");
        assert_eq!(fee.add(&net), d("1.0000001"));
    }

    #[test]
    fn smallest_amount_still_pays_a_unit_of_fee() {
        let fee = FeeSchedule::bps(1).fee_for(&d("0.0000001"), &RoundingPolicy::STANDARD);
        assert_eq!(fee.to_string(), "0.0000001");
    }

    #[test]
    fn flat_fee_is_added_and_capped() {
        let p = RoundingPolicy::STANDARD;
        let s = FeeSchedule::bps(10).with_flat(d("0.5"));
        assert_eq!(s.fee_for(&d("100.0000000"), &p).to_string(), "0.6000000");
        assert_eq!(s.fee_for(&d("0.1000000"), &p).to_string(), "0.1000000");
    }
}
