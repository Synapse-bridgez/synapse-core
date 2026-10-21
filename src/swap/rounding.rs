//! Explicit rounding policy for the Swap Engine.
//!
//! Every lossy step in a swap computation names *why* it is rounding (a
//! [`Purpose`]) and asks the shared [`RoundingPolicy`] for the mode, instead
//! of picking a mode at the call site. The default policy,
//! [`RoundingPolicy::STANDARD`], follows one rule:
//!
//! > **Never promise or pay the client more than the exact value, never
//! > charge them less than the exact fee, and make every protection check
//! > err on the side of rejecting.**
//!
//! | Purpose        | Mode       | Why                                                      |
//! |----------------|------------|----------------------------------------------------------|
//! | `QuoteOutput`  | `Down`     | A quote never promises more than the route can deliver.  |
//! | `QuoteInput`   | `Up`       | A required input is never understated.                   |
//! | `Fee`          | `Up`       | Fees are never undercharged by rounding.                 |
//! | `Fill`         | `Down`     | A fill never credits more than was actually received.    |
//! | `Allocation`   | `Down`     | Shares round down; the remainder is distributed exactly (see [`allocation`](crate::swap::allocation)). |
//! | `Rate`         | `HalfEven` | Displayed/stored rates are unbiased; they never move money. |
//! | `PriceImpact`  | `Up`       | Overstating impact makes the tolerance check fail safe.  |
//! | `ProtectionFloor` | `Up`    | The minimum-acceptable output is never loosened by rounding. |

use serde::{Deserialize, Serialize};

/// How to round a value that does not fit the target scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoundingMode {
    /// Toward zero (truncate).
    Down,
    /// Away from zero.
    Up,
    /// Toward negative infinity.
    Floor,
    /// Toward positive infinity.
    Ceiling,
    /// Nearest; ties away from zero.
    HalfUp,
    /// Nearest; ties toward zero.
    HalfDown,
    /// Nearest; ties to the even neighbour (banker's rounding).
    HalfEven,
}

/// The reason a computation rounds. See the module docs for the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Purpose {
    QuoteOutput,
    QuoteInput,
    Fee,
    Fill,
    Allocation,
    Rate,
    PriceImpact,
    ProtectionFloor,
}

/// Mapping from [`Purpose`] to [`RoundingMode`]. Construct a custom one only
/// for tests; production code uses [`RoundingPolicy::STANDARD`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoundingPolicy {
    pub quote_output: RoundingMode,
    pub quote_input: RoundingMode,
    pub fee: RoundingMode,
    pub fill: RoundingMode,
    pub allocation: RoundingMode,
    pub rate: RoundingMode,
    pub price_impact: RoundingMode,
    pub protection_floor: RoundingMode,
}

impl RoundingPolicy {
    pub const STANDARD: RoundingPolicy = RoundingPolicy {
        quote_output: RoundingMode::Down,
        quote_input: RoundingMode::Up,
        fee: RoundingMode::Up,
        fill: RoundingMode::Down,
        allocation: RoundingMode::Down,
        rate: RoundingMode::HalfEven,
        price_impact: RoundingMode::Up,
        protection_floor: RoundingMode::Up,
    };

    pub const fn mode(&self, purpose: Purpose) -> RoundingMode {
        match purpose {
            Purpose::QuoteOutput => self.quote_output,
            Purpose::QuoteInput => self.quote_input,
            Purpose::Fee => self.fee,
            Purpose::Fill => self.fill,
            Purpose::Allocation => self.allocation,
            Purpose::Rate => self.rate,
            Purpose::PriceImpact => self.price_impact,
            Purpose::ProtectionFloor => self.protection_floor,
        }
    }
}

impl Default for RoundingPolicy {
    fn default() -> Self {
        Self::STANDARD
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_policy_is_conservative_for_the_client() {
        let p = RoundingPolicy::STANDARD;
        assert_eq!(p.mode(Purpose::QuoteOutput), RoundingMode::Down);
        assert_eq!(p.mode(Purpose::Fill), RoundingMode::Down);
        assert_eq!(p.mode(Purpose::Fee), RoundingMode::Up);
        assert_eq!(p.mode(Purpose::QuoteInput), RoundingMode::Up);
        assert_eq!(p.mode(Purpose::PriceImpact), RoundingMode::Up);
        assert_eq!(p.mode(Purpose::ProtectionFloor), RoundingMode::Up);
        assert_eq!(p.mode(Purpose::Rate), RoundingMode::HalfEven);
        assert_eq!(RoundingPolicy::default(), p);
    }
}
