//! Deterministic split allocation.
//!
//! Allocation is performed in integer units (10^-7 of the sell asset), never
//! floating point. Given the same request and the same source snapshot, the
//! leg amounts are identical regardless of input order:
//!
//! 1. sources are ordered by `(priority, id)`;
//! 2. proportional floors are allocated by weight, capped by each source's
//!    available/max amount;
//! 3. the leftover units are handed out by largest remainder, then source
//!    order, so ties never depend on hash-map iteration;
//! 4. sources that would receive a dust leg below their `min_leg_amount` are
//!    removed and the amount is redistributed.
//!
//! The sum of all legs always equals the requested amount exactly.

use bigdecimal::BigDecimal;
use std::collections::HashSet;

use super::types::{
    LiquiditySource, SwapOrderError, SwapOrderResult, AMOUNT_SCALE, UNIT_SCALE_FACTOR,
};

/// Upper bound on allocation rounds. Each round either fills a leg to its cap,
/// removes an unviable dust leg, or distributes at least one unit, so this is
/// a defensive guard rather than a normal exit path.
const MAX_ALLOCATION_ROUNDS_PER_SOURCE: usize = 8;

#[derive(Debug, Clone)]
struct AllocInput {
    id: String,
    priority: u32,
    weight: u32,
    capacity: i64,
    min_leg: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allocation {
    pub source_id: String,
    pub priority: u32,
    pub amount_units: i64,
}

/// Allocate `total_units` across `sources`, returning one entry per eligible
/// source (including sources that legitimately receive zero because their
/// weight is zero).
pub fn allocate_deterministic(
    total_units: i64,
    sources: &[LiquiditySource],
) -> SwapOrderResult<Vec<Allocation>> {
    if total_units <= 0 {
        return Err(SwapOrderError::Validation(
            "sell_amount must be positive".to_string(),
        ));
    }
    if sources.is_empty() {
        return Err(SwapOrderError::Validation(
            "at least one liquidity source is required".to_string(),
        ));
    }

    let mut seen = HashSet::new();
    let mut inputs = Vec::with_capacity(sources.len());
    for source in sources {
        if source.id.trim().is_empty() {
            return Err(SwapOrderError::Validation(
                "liquidity source id must not be empty".to_string(),
            ));
        }
        if !seen.insert(source.id.clone()) {
            return Err(SwapOrderError::Validation(format!(
                "duplicate liquidity source id: {}",
                source.id
            )));
        }
        if source.price <= BigDecimal::from(0) {
            return Err(SwapOrderError::Validation(format!(
                "source {} has a non-positive price",
                source.id
            )));
        }
        if source.fee_bps > 10_000 {
            return Err(SwapOrderError::Validation(format!(
                "source {} fee_bps must be at most 10000",
                source.id
            )));
        }

        let available = to_units(&source.available)?;
        let max_leg = match &source.max_leg_amount {
            Some(value) => Some(to_units(value)?),
            None => None,
        };
        let min_leg = match &source.min_leg_amount {
            Some(value) => Some(to_units(value)?),
            None => None,
        };
        let capacity = match max_leg {
            Some(max) => available.min(max).max(0),
            None => available.max(0),
        };

        inputs.push(AllocInput {
            id: source.id.clone(),
            priority: source.priority,
            weight: source.weight,
            capacity,
            min_leg,
        });
    }

    inputs.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| a.id.cmp(&b.id))
    });

    let mut excluded: HashSet<String> = HashSet::new();
    let max_outer_rounds = inputs.len() + 1;

    for _ in 0..max_outer_rounds {
        let eligible: Vec<&AllocInput> = inputs
            .iter()
            .filter(|input| !excluded.contains(&input.id))
            .collect();

        if eligible.is_empty() {
            return Err(SwapOrderError::InsufficientLiquidity {
                requested: from_units(total_units),
                available: BigDecimal::from(0),
            });
        }

        let capacities: Vec<i64> = eligible
            .iter()
            .map(|input| {
                if input.weight == 0 {
                    0
                } else {
                    input.capacity.max(0)
                }
            })
            .collect();

        let total_capacity: i128 = capacities.iter().map(|capacity| *capacity as i128).sum();
        if total_capacity < total_units as i128 {
            return Err(SwapOrderError::InsufficientLiquidity {
                requested: from_units(total_units),
                available: from_units(
                    total_capacity.min(i64::MAX as i128).max(0) as i64,
                ),
            });
        }

        let amounts = allocate_once(
            total_units,
            &eligible
                .iter()
                .map(|input| input.weight)
                .collect::<Vec<_>>(),
            &capacities,
            &eligible.iter().map(|input| input.id.clone()).collect::<Vec<_>>(),
        )?;

        let dust: Vec<String> = eligible
            .iter()
            .zip(amounts.iter())
            .filter(|(input, amount)| {
                input
                    .min_leg
                    .map(|min| **amount > 0 && **amount < min)
                    .unwrap_or(false)
            })
            .map(|(input, _)| input.id.clone())
            .collect();

        if dust.is_empty() {
            return Ok(eligible
                .into_iter()
                .zip(amounts)
                .map(|(input, amount_units)| Allocation {
                    source_id: input.id.clone(),
                    priority: input.priority,
                    amount_units,
                })
                .collect());
        }

        // A leg below the source's minimum is not viable. Remove it and
        // redistribute instead of emitting dust. This can cascade, but the
        // outer bound guarantees termination; if it cannot be satisfied the
        // request fails rather than silently producing a short allocation.
        for id in dust {
            excluded.insert(id);
        }
    }

    Err(SwapOrderError::Validation(
        "liquidity source minimum leg sizes cannot be satisfied".to_string(),
    ))
}

fn allocate_once(
    total_units: i64,
    weights: &[u32],
    capacities: &[i64],
    ids: &[String],
) -> SwapOrderResult<Vec<i64>> {
    let mut allocated = vec![0_i64; weights.len()];
    let mut remaining = total_units as i128;
    let max_rounds = weights.len() * MAX_ALLOCATION_ROUNDS_PER_SOURCE + 16;

    for _ in 0..max_rounds {
        if remaining == 0 {
            break;
        }

        let active: Vec<usize> = (0..weights.len())
            .filter(|idx| weights[*idx] > 0 && allocated[*idx] < capacities[*idx])
            .collect();

        if active.is_empty() {
            return Err(SwapOrderError::InsufficientLiquidity {
                requested: from_units(total_units),
                available: from_units(allocated.iter().sum()),
            });
        }

        let total_weight: u128 = active.iter().map(|idx| weights[*idx] as u128).sum();
        let mut added = 0_i128;
        let mut remainders: Vec<(u128, usize)> = Vec::with_capacity(active.len());

        for idx in &active {
            let numerator = remaining as u128 * weights[*idx] as u128;
            let mut share = (numerator / total_weight) as i128;
            let remainder = numerator % total_weight;
            let room = (capacities[*idx] - allocated[*idx]) as i128;
            if share > room {
                share = room;
            }
            allocated[*idx] += share as i64;
            added += share;
            remainders.push((remainder, *idx));
        }

        remaining -= added;

        if remaining > 0 && added == 0 {
            // Proportional floors were all zero. Allocate the remaining
            // units by largest remainder, then by stable source order.
            remainders.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            for (_, idx) in remainders {
                if remaining == 0 {
                    break;
                }
                if allocated[idx] < capacities[idx] {
                    allocated[idx] += 1;
                    remaining -= 1;
                }
            }
        }
    }

    if remaining != 0 {
        return Err(SwapOrderError::Internal(format!(
            "allocation left {remaining} units unassigned across {ids:?}"
        )));
    }

    Ok(allocated)
}

/// Convert a decimal amount to scaled integer units, truncating toward zero.
/// Rejects negative amounts and values that overflow the supported range.
pub fn to_units(amount: &BigDecimal) -> SwapOrderResult<i64> {
    if amount < &BigDecimal::from(0) {
        return Err(SwapOrderError::Validation(
            "amount must not be negative".to_string(),
        ));
    }

    let scaled = amount.with_scale(AMOUNT_SCALE);
    let text = scaled.to_string();
    let (whole, fraction) = text.split_once('.').unwrap_or((text.as_str(), ""));
    if whole.contains(['e', 'E']) || fraction.contains(['e', 'E']) {
        return Err(SwapOrderError::Validation(format!(
            "amount is outside the supported range: {amount}"
        )));
    }

    let whole: i128 = whole
        .parse()
        .map_err(|_| SwapOrderError::Validation(format!("invalid amount: {amount}")))?;
    let mut fraction = fraction.to_string();
    fraction.truncate(AMOUNT_SCALE as usize);
    while fraction.len() < AMOUNT_SCALE as usize {
        fraction.push('0');
    }
    let fraction: i128 = fraction
        .parse()
        .map_err(|_| SwapOrderError::Validation(format!("invalid amount: {amount}")))?;

    let units = whole
        .checked_mul(UNIT_SCALE_FACTOR as i128)
        .and_then(|value| value.checked_add(fraction))
        .ok_or_else(|| {
            SwapOrderError::Validation(format!("amount is outside the supported range: {amount}"))
        })?;

    i64::try_from(units)
        .map_err(|_| SwapOrderError::Validation(format!("amount is outside the supported range: {amount}")))
}

/// Convert scaled integer units back to a decimal amount.
pub fn from_units(units: i64) -> BigDecimal {
    let sign = if units < 0 { "-" } else { "" };
    let absolute = i128::from(units).abs();
    let whole = absolute / UNIT_SCALE_FACTOR as i128;
    let fraction = absolute % UNIT_SCALE_FACTOR as i128;
    format!("{sign}{whole}.{fraction:07}")
        .parse()
        .expect("formatted fixed-point amount is always valid BigDecimal")
}

/// Quantize an amount to the swap precision, truncating toward zero. Use this
/// for every multiplication so estimates never round *up* a reservation.
pub fn quantize_down(amount: &BigDecimal) -> BigDecimal {
    amount.with_scale(AMOUNT_SCALE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swap::types::LiquiditySourceKind;

    fn source(id: &str, weight: u32, available: i64) -> LiquiditySource {
        LiquiditySource {
            id: id.to_string(),
            kind: LiquiditySourceKind::StellarDex,
            price: BigDecimal::from(1),
            available: from_units(available),
            weight,
            priority: 0,
            min_leg_amount: None,
            max_leg_amount: None,
            fee_bps: 0,
        }
    }

    fn amounts(allocations: &[Allocation]) -> Vec<i64> {
        allocations
            .iter()
            .map(|allocation| allocation.amount_units)
            .collect()
    }

    #[test]
    fn to_units_and_from_units_round_trip() {
        for value in ["0.0000001", "1", "1.2345678", "999999.9999999"] {
            let decimal: BigDecimal = value.parse().unwrap();
            let units = to_units(&decimal).unwrap();
            assert_eq!(
                from_units(units).with_scale(AMOUNT_SCALE),
                decimal.with_scale(AMOUNT_SCALE)
            );
        }
    }

    #[test]
    fn to_units_rejects_negative_amounts() {
        let amount: BigDecimal = "-0.0000001".parse().unwrap();
        assert!(matches!(
            to_units(&amount),
            Err(SwapOrderError::Validation(_))
        ));
    }

    #[test]
    fn allocation_is_exact_and_deterministic() {
        let sources = vec![
            source("b", 1, 1_000_000_000),
            source("a", 1, 1_000_000_000),
            source("c", 1, 1_000_000_000),
        ];
        let first = allocate_deterministic(100_0000_000, &sources).unwrap();
        // Re-ordering the request must not change any leg amount.
        let reordered = vec![
            sources[2].clone(),
            sources[0].clone(),
            sources[1].clone(),
        ];
        let second = allocate_deterministic(100_0000_000, &reordered).unwrap();
        assert_eq!(first, second);
        assert_eq!(
            amounts(&first).iter().sum::<i64>(),
            100_0000_000,
            "allocation must be exact"
        );
        // Largest-remainder tie-break hands the leftover unit to the lowest
        // source id (`a`), never to map iteration order.
        assert_eq!(
            amounts(&first),
            vec![33_3333_334, 33_3333_333, 33_3333_333]
        );
    }

    #[test]
    fn capacity_is_respected_and_remainder_redistributed() {
        let mut small = source("a", 1, 10_0000_000);
        small.priority = 0;
        let mut large = source("b", 1, 100_0000_000);
        large.priority = 1;
        let allocations =
            allocate_deterministic(50_0000_000, &[small, large]).unwrap();
        assert_eq!(amounts(&allocations), vec![10_0000_000, 40_0000_000]);
    }

    #[test]
    fn max_leg_cap_is_applied_before_weighting() {
        let mut capped = source("a", 1, 100_0000_000);
        capped.max_leg_amount = Some(from_units(20_0000_000));
        let b = source("b", 1, 100_0000_000);
        let allocations = allocate_deterministic(60_0000_000, &[capped, b]).unwrap();
        assert_eq!(amounts(&allocations), vec![20_0000_000, 40_0000_000]);
    }

    #[test]
    fn dust_leg_is_removed_and_redistributed() {
        let mut a = source("a", 89, 100_0000_000);
        a.min_leg_amount = Some(from_units(5_0000_000));
        let mut b = source("b", 11, 100_0000_000);
        b.min_leg_amount = Some(from_units(5_0000_000));
        let allocations = allocate_deterministic(10_0000_000, &[a, b]).unwrap();
        assert_eq!(amounts(&allocations), vec![10_0000_000, 0]);
    }

    #[test]
    fn insufficient_liquidity_is_reported_as_such() {
        let source = source("a", 1, 5_0000_000);
        let error = allocate_deterministic(10_0000_000, &[source]).unwrap_err();
        assert!(matches!(
            error,
            SwapOrderError::InsufficientLiquidity { .. }
        ));
    }

    #[test]
    fn zero_weight_sources_receive_nothing() {
        let a = source("a", 0, 100_0000_000);
        let b = source("b", 1, 100_0000_000);
        let allocations = allocate_deterministic(10_0000_000, &[a, b]).unwrap();
        assert_eq!(amounts(&allocations), vec![0, 10_0000_000]);
    }

    #[test]
    fn duplicate_source_ids_are_rejected() {
        let a = source("a", 1, 100_0000_000);
        let b = source("a", 1, 100_0000_000);
        assert!(matches!(
            allocate_deterministic(1_0000_000, &[a, b]),
            Err(SwapOrderError::Validation(_))
        ));
    }

    #[test]
    fn quantization_truncates_toward_zero() {
        let amount: BigDecimal = "1.12345678".parse().unwrap();
        assert_eq!(quantize_down(&amount), "1.1234567".parse().unwrap());
    }
}
