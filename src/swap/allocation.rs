//! Deterministic, exact allocation of an amount across parts.
//!
//! Used whenever one amount is split — across liquidity providers, partial
//! fills, or fee recipients. Each share is rounded with the policy's
//! [`Purpose::Allocation`] mode (down), then the leftover units are handed
//! out one at a time by largest remainder, ties broken by lowest index. The
//! shares therefore always sum to exactly the input, and the same input
//! always produces the same split.

use num_bigint::BigInt;
use num_integer::Integer;
use num_traits::{Signed, Zero};

use crate::swap::decimal::{round_div, Decimal};
use crate::swap::rounding::{Purpose, RoundingPolicy};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AllocationError {
    #[error("allocation needs at least one weight")]
    NoWeights,
    #[error("allocation weights must be non-negative and not all zero")]
    InvalidWeights,
    #[error("cannot allocate a negative amount")]
    NegativeAmount,
}

/// Split `total` proportionally to `weights`, at `total`'s scale.
pub fn allocate(
    total: &Decimal,
    weights: &[Decimal],
    policy: &RoundingPolicy,
) -> Result<Vec<Decimal>, AllocationError> {
    if weights.is_empty() {
        return Err(AllocationError::NoWeights);
    }
    if total.is_negative() {
        return Err(AllocationError::NegativeAmount);
    }
    if weights.iter().any(Decimal::is_negative) || weights.iter().all(Decimal::is_zero) {
        return Err(AllocationError::InvalidWeights);
    }
    // Bring weights to integers at a common scale.
    let wscale = weights.iter().map(Decimal::scale).max().unwrap_or(0);
    let w: Vec<BigInt> = weights
        .iter()
        .map(|x| {
            x.rescale(wscale, policy.mode(Purpose::Allocation))
                .units()
                .clone()
        })
        .collect();
    let sum_w: BigInt = w.iter().sum();
    let units = total.units();
    let scale = total.scale();
    let mode = policy.mode(Purpose::Allocation);

    let mut shares: Vec<BigInt> = Vec::with_capacity(w.len());
    let mut remainders: Vec<(BigInt, usize)> = Vec::with_capacity(w.len());
    for (i, wi) in w.iter().enumerate() {
        let num = units * wi;
        let share = round_div(&num, &sum_w, mode);
        let rem = num.mod_floor(&sum_w);
        shares.push(share);
        remainders.push((rem, i));
    }
    let allocated: BigInt = shares.iter().sum();
    let mut leftover = units - allocated;
    // Largest remainder first; ties by lowest index.
    remainders.sort_by(|(ra, ia), (rb, ib)| rb.cmp(ra).then(ia.cmp(ib)));
    let mut k = 0;
    while leftover.is_positive() {
        let idx = remainders[k % remainders.len()].1;
        if !w[idx].is_zero() {
            shares[idx] += 1;
            leftover -= 1;
        }
        k += 1;
    }
    Ok(shares
        .into_iter()
        .map(|u| Decimal::from_units(u, scale))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        Decimal::parse(s).unwrap()
    }

    fn total(v: &[Decimal]) -> Decimal {
        v.iter().fold(Decimal::zero(0), |a, b| a.add(b))
    }

    #[test]
    fn thirds_sum_exactly() {
        let p = RoundingPolicy::STANDARD;
        let parts = allocate(&d("1.0000000"), &[d("1"), d("1"), d("1")], &p).unwrap();
        let s: Vec<String> = parts.iter().map(|x| x.to_string()).collect();
        assert_eq!(s, ["0.3333334", "0.3333333", "0.3333333"]);
        assert_eq!(total(&parts), d("1"));
    }

    #[test]
    fn largest_remainder_wins_the_leftover_unit() {
        let p = RoundingPolicy::STANDARD;
        let parts = allocate(&d("0.0000010"), &[d("1"), d("2"), d("4")], &p).unwrap();
        // exact: 1.428.., 2.857.., 5.714.. units -> 1, 3, 6
        let s: Vec<String> = parts.iter().map(|x| x.to_string()).collect();
        assert_eq!(s, ["0.0000001", "0.0000003", "0.0000006"]);
    }

    #[test]
    fn zero_weight_gets_nothing() {
        let p = RoundingPolicy::STANDARD;
        let parts = allocate(&d("0.0000003"), &[d("0"), d("1"), d("1")], &p).unwrap();
        assert!(parts[0].is_zero());
        assert_eq!(total(&parts), d("0.0000003"));
    }

    #[test]
    fn rejects_bad_input() {
        let p = RoundingPolicy::STANDARD;
        assert_eq!(allocate(&d("1"), &[], &p), Err(AllocationError::NoWeights));
        assert_eq!(
            allocate(&d("1"), &[d("0")], &p),
            Err(AllocationError::InvalidWeights)
        );
        assert_eq!(
            allocate(&d("-1"), &[d("1")], &p),
            Err(AllocationError::NegativeAmount)
        );
    }

    proptest::proptest! {
        #[test]
        fn allocation_always_sums_to_total(units in 0u64..10_000_000_000, ws in proptest::collection::vec(0u32..1000, 1..8)) {
            proptest::prop_assume!(ws.iter().any(|w| *w > 0));
            let p = RoundingPolicy::STANDARD;
            let t = Decimal::from_units(units, 7);
            let weights: Vec<Decimal> = ws.iter().map(|w| Decimal::from_units(*w, 0)).collect();
            let parts = allocate(&t, &weights, &p).unwrap();
            proptest::prop_assert_eq!(total(&parts), t);
            for (part, w) in parts.iter().zip(&ws) {
                proptest::prop_assert!(!part.is_negative());
                if *w == 0 { proptest::prop_assert!(part.is_zero()); }
            }
        }
    }
}
