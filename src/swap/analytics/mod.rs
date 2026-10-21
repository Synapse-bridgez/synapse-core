//! Best-execution / transaction cost analysis for swap fills.
//!
//! Raw inputs (reference price, decision price, fill price, fees) are stored in
//! `swap_fill_tca` and metrics are derived here at query time, so the
//! calculation can be corrected without rewriting history.
//!
//! Reference price policy: use the quorum (median) price when >= 2 sources are
//! available, else the single oracle; a snapshot older than `max_age_secs` at
//! quote time is treated as missing and every metric is `None` (null-safe).
//! Cancelled / unfilled orders are excluded from aggregates.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FillInputs {
    pub side: Side,
    /// Mid-market reference at decision time; `None` if missing/stale.
    pub reference_price: Option<f64>,
    pub fill_price: f64,
    pub quantity: f64,
    pub lp_fee: f64,
    pub network_fee: f64,
    /// Price observed immediately after the fill, for price impact.
    pub post_fill_price: Option<f64>,
    pub filled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FillMetrics {
    /// Signed slippage vs reference in bps; positive = worse than reference.
    pub slippage_bps: Option<f64>,
    pub price_impact_bps: Option<f64>,
    /// Cost of price deviation in quote units (positive = cost).
    pub spread_cost: Option<f64>,
    /// spread_cost + fees; fees alone when the reference is missing.
    pub total_cost: f64,
}

/// Selects the reference price per the documented policy.
pub fn select_reference(mut prices: Vec<f64>, age_secs: i64, max_age_secs: i64) -> Option<f64> {
    prices.retain(|p| p.is_finite() && *p > 0.0);
    if age_secs > max_age_secs || prices.is_empty() {
        return None;
    }
    prices.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = prices.len();
    Some(if n % 2 == 1 {
        prices[n / 2]
    } else {
        (prices[n / 2 - 1] + prices[n / 2]) / 2.0
    })
}

fn bps(delta: f64, base: f64) -> f64 {
    delta / base * 10_000.0
}

/// `None` for cancelled/unfilled orders.
pub fn compute(i: &FillInputs) -> Option<FillMetrics> {
    if !i.filled {
        return None;
    }
    let sign = match i.side {
        Side::Buy => 1.0,
        Side::Sell => -1.0,
    };
    let fees = i.lp_fee + i.network_fee;
    let reference = i.reference_price.filter(|r| *r > 0.0);
    let slippage_bps = reference.map(|r| sign * bps(i.fill_price - r, r));
    let spread_cost = reference.map(|r| sign * (i.fill_price - r) * i.quantity);
    let price_impact_bps = i
        .post_fill_price
        .filter(|_| i.fill_price > 0.0)
        .map(|p| sign * bps(p - i.fill_price, i.fill_price));
    Some(FillMetrics {
        slippage_bps,
        price_impact_bps,
        spread_cost,
        total_cost: spread_cost.unwrap_or(0.0) + fees,
    })
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Aggregate {
    pub fills: usize,
    pub avg_slippage_bps: Option<f64>,
    pub total_cost: f64,
}

/// Aggregate over fills; unfilled orders are excluded and fills with a missing
/// reference contribute to cost but not to the slippage average.
pub fn aggregate(fills: &[FillInputs]) -> Aggregate {
    let mut agg = Aggregate::default();
    let (mut sum, mut n) = (0.0, 0usize);
    for m in fills.iter().filter_map(compute) {
        agg.fills += 1;
        agg.total_cost += m.total_cost;
        if let Some(s) = m.slippage_bps {
            sum += s;
            n += 1;
        }
    }
    agg.avg_slippage_bps = (n > 0).then(|| sum / n as f64);
    agg
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(side: Side, reference: Option<f64>, fill_price: f64) -> FillInputs {
        FillInputs {
            side,
            reference_price: reference,
            fill_price,
            quantity: 100.0,
            lp_fee: 0.5,
            network_fee: 0.1,
            post_fill_price: None,
            filled: true,
        }
    }

    #[test]
    fn buy_above_reference_is_positive_slippage() {
        let m = compute(&fill(Side::Buy, Some(1.0), 1.001)).unwrap();
        assert!((m.slippage_bps.unwrap() - 10.0).abs() < 1e-9);
        assert!((m.total_cost - (0.1 + 0.6)).abs() < 1e-9);
    }

    #[test]
    fn sell_below_reference_is_positive_slippage() {
        let m = compute(&fill(Side::Sell, Some(1.0), 0.999)).unwrap();
        assert!((m.slippage_bps.unwrap() - 10.0).abs() < 1e-9);
    }

    #[test]
    fn negative_slippage_price_improvement() {
        let m = compute(&fill(Side::Buy, Some(1.0), 0.999)).unwrap();
        assert!(m.slippage_bps.unwrap() < 0.0);
    }

    #[test]
    fn zero_spread() {
        let m = compute(&fill(Side::Buy, Some(1.0), 1.0)).unwrap();
        assert_eq!(m.slippage_bps, Some(0.0));
        assert!((m.total_cost - 0.6).abs() < 1e-9);
    }

    #[test]
    fn missing_reference_is_null_safe() {
        let m = compute(&fill(Side::Buy, None, 1.0)).unwrap();
        assert_eq!(m.slippage_bps, None);
        assert_eq!(m.spread_cost, None);
        assert!((m.total_cost - 0.6).abs() < 1e-9);
    }

    #[test]
    fn unfilled_excluded() {
        let mut f = fill(Side::Buy, Some(1.0), 1.0);
        f.filled = false;
        assert!(compute(&f).is_none());
        assert_eq!(aggregate(&[f]).fills, 0);
    }

    #[test]
    fn aggregates_two_providers() {
        let a = aggregate(&[
            fill(Side::Buy, Some(1.0), 1.001),
            fill(Side::Buy, Some(1.0), 1.003),
            fill(Side::Buy, None, 1.0),
        ]);
        assert_eq!(a.fills, 3);
        assert!((a.avg_slippage_bps.unwrap() - 20.0).abs() < 1e-9);
    }

    #[test]
    fn reference_policy() {
        assert_eq!(select_reference(vec![1.0, 3.0, 2.0], 1, 5), Some(2.0));
        assert_eq!(select_reference(vec![1.0, 2.0], 1, 5), Some(1.5));
        assert_eq!(select_reference(vec![1.0], 10, 5), None);
        assert_eq!(select_reference(vec![], 0, 5), None);
        assert_eq!(select_reference(vec![-1.0, 0.0], 0, 5), None);
    }
}
