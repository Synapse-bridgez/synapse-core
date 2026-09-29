//! Regression tests for the release scorecard.
//!
//! Production metrics are autocorrelated (a slow minute is usually followed
//! by another), so treating every scrape step as an independent sample
//! wildly overstates significance. Samples are therefore first averaged into
//! blocks (default: 1 hour of 5-minute steps) and the blocks are compared.
//!
//! * Continuous metrics (error rate, latency quantiles): one-sided
//!   Mann-Whitney U test on the blocks — no normality assumption, robust to
//!   the long tails latency always has — plus a minimum effect size, so a
//!   statistically detectable but operationally meaningless shift is not
//!   flagged.
//! * Incident counts: exact conditional binomial test comparing the two
//!   Poisson rates, which stays valid for the small counts incidents come in.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Worse after the release by a statistically meaningful margin.
    Regression,
    /// Better after the release by a statistically meaningful margin.
    Improvement,
    /// Any difference is within noise.
    NoSignificantChange,
    /// Not enough data in one of the windows to say.
    InsufficientData,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Regression => "🔴 regression",
            Verdict::Improvement => "🟢 improvement",
            Verdict::NoSignificantChange => "⚪ within noise",
            Verdict::InsufficientData => "⚠️ insufficient data",
        }
    }
}

/// Thresholds for flagging a continuous metric.
#[derive(Debug, Clone, Copy)]
pub struct Thresholds {
    /// One-sided significance level.
    pub alpha: f64,
    /// Minimum relative change of the block medians.
    pub min_relative_change: f64,
    /// Minimum absolute change of the block medians (metric units).
    pub min_absolute_change: f64,
    /// Minimum blocks per window.
    pub min_blocks: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Comparison {
    pub before: Option<f64>,
    pub after: Option<f64>,
    pub relative_change: Option<f64>,
    /// One-sided p-value in the direction of the observed change.
    pub p_value: Option<f64>,
    pub verdict: Verdict,
}

/// Averages consecutive samples into blocks of `block_size`; a trailing
/// partial block is kept if at least half full.
pub fn block_means(samples: &[f64], block_size: usize) -> Vec<f64> {
    let block_size = block_size.max(1);
    samples
        .chunks(block_size)
        .filter(|c| c.len() * 2 >= block_size)
        .map(|c| c.iter().sum::<f64>() / c.len() as f64)
        .collect()
}

pub fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    let mid = v.len() / 2;
    Some(if v.len().is_multiple_of(2) {
        (v[mid - 1] + v[mid]) / 2.0
    } else {
        v[mid]
    })
}

/// Standard normal CDF (Abramowitz & Stegun 7.1.26, |error| < 1.5e-7).
pub fn normal_cdf(z: f64) -> f64 {
    let x = z.abs() / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let poly = t
        * (0.254_829_592
            + t * (-0.284_496_736
                + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    let erf = 1.0 - poly * (-x * x).exp();
    if z >= 0.0 {
        0.5 * (1.0 + erf)
    } else {
        0.5 * (1.0 - erf)
    }
}

/// One-sided Mann-Whitney U test, H1: `after` tends to be larger than
/// `before`. Normal approximation with tie and continuity correction.
/// Returns 1.0 when either side is empty or all values are tied.
pub fn mann_whitney_greater(before: &[f64], after: &[f64]) -> f64 {
    let (nb, na) = (before.len() as f64, after.len() as f64);
    if before.is_empty() || after.is_empty() {
        return 1.0;
    }
    let mut all: Vec<(f64, bool)> = before
        .iter()
        .map(|v| (*v, false))
        .chain(after.iter().map(|v| (*v, true)))
        .collect();
    all.sort_by(|a, b| a.0.total_cmp(&b.0));

    let n = all.len();
    let mut rank_sum_after = 0.0;
    let mut tie_term = 0.0;
    let mut i = 0;
    while i < n {
        let mut j = i;
        while j + 1 < n && all[j + 1].0 == all[i].0 {
            j += 1;
        }
        let avg_rank = (i + j) as f64 / 2.0 + 1.0;
        let t = (j - i + 1) as f64;
        tie_term += t * t * t - t;
        rank_sum_after += all[i..=j].iter().filter(|x| x.1).count() as f64 * avg_rank;
        i = j + 1;
    }

    let n = n as f64;
    let u = rank_sum_after - na * (na + 1.0) / 2.0;
    let mean = na * nb / 2.0;
    let var = na * nb / 12.0 * ((n + 1.0) - tie_term / (n * (n - 1.0)));
    if var <= 0.0 {
        return 1.0;
    }
    let z = (u - mean - 0.5) / var.sqrt();
    1.0 - normal_cdf(z)
}

/// Compares a higher-is-worse metric (error rate, latency) between windows.
pub fn compare_continuous(before: &[f64], after: &[f64], th: Thresholds) -> Comparison {
    let before_med = median(before);
    let after_med = median(after);
    let relative_change = match (before_med, after_med) {
        (Some(b), Some(a)) if b.abs() > f64::EPSILON => Some((a - b) / b),
        (Some(_), Some(a)) if a.abs() > f64::EPSILON => Some(f64::INFINITY),
        (Some(_), Some(_)) => Some(0.0),
        _ => None,
    };
    let mut out = Comparison {
        before: before_med,
        after: after_med,
        relative_change,
        p_value: None,
        verdict: Verdict::InsufficientData,
    };
    if before.len() < th.min_blocks || after.len() < th.min_blocks {
        return out;
    }
    let (b, a) = (before_med.unwrap_or(0.0), after_med.unwrap_or(0.0));
    let worse = a >= b;
    let p = if worse {
        mann_whitney_greater(before, after)
    } else {
        mann_whitney_greater(after, before)
    };
    out.p_value = Some(p);
    let big_enough = (a - b).abs() >= th.min_absolute_change
        && relative_change.is_some_and(|r| r.abs() >= th.min_relative_change);
    out.verdict = if p < th.alpha && big_enough {
        if worse {
            Verdict::Regression
        } else {
            Verdict::Improvement
        }
    } else {
        Verdict::NoSignificantChange
    };
    out
}

/// `P(X >= k)` for `X ~ Binomial(n, p)`, computed in log space.
pub fn binomial_upper_tail(k: u64, n: u64, p: f64) -> f64 {
    if k == 0 {
        return 1.0;
    }
    if k > n || p <= 0.0 {
        return 0.0;
    }
    if p >= 1.0 {
        return 1.0;
    }
    let (ln_p, ln_q) = (p.ln(), (1.0 - p).ln());
    let mut ln_pmf = n as f64 * ln_q; // k = 0
    let mut tail = 0.0;
    for i in 0..n {
        // ln pmf(i + 1) from ln pmf(i)
        ln_pmf += ((n - i) as f64).ln() - ((i + 1) as f64).ln() + ln_p - ln_q;
        if i + 1 >= k {
            tail += ln_pmf.exp();
        }
    }
    tail.min(1.0)
}

/// Compares incident counts over windows of (possibly) different lengths.
pub fn compare_counts(
    before: u64,
    before_hours: f64,
    after: u64,
    after_hours: f64,
    alpha: f64,
) -> Comparison {
    let mut out = Comparison {
        before: Some(before as f64),
        after: Some(after as f64),
        relative_change: None,
        p_value: None,
        verdict: Verdict::InsufficientData,
    };
    if before_hours <= 0.0 || after_hours <= 0.0 {
        return out;
    }
    let (rate_b, rate_a) = (before as f64 / before_hours, after as f64 / after_hours);
    out.relative_change = if rate_b > 0.0 {
        Some((rate_a - rate_b) / rate_b)
    } else if rate_a > 0.0 {
        Some(f64::INFINITY)
    } else {
        Some(0.0)
    };
    let n = before + after;
    let share_after = after_hours / (before_hours + after_hours);
    let worse = rate_a >= rate_b;
    let p = if worse {
        binomial_upper_tail(after, n, share_after)
    } else {
        binomial_upper_tail(before, n, 1.0 - share_after)
    };
    out.p_value = Some(p);
    out.verdict = if n > 0 && p < alpha {
        if worse {
            Verdict::Regression
        } else {
            Verdict::Improvement
        }
    } else {
        Verdict::NoSignificantChange
    };
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TH: Thresholds = Thresholds {
        alpha: 0.01,
        min_relative_change: 0.10,
        min_absolute_change: 0.0,
        min_blocks: 6,
    };

    /// Deterministic pseudo-noise in [-1, 1] (xorshift) so tests don't need
    /// a rand dependency.
    fn noise(seed: u64, n: usize) -> Vec<f64> {
        let mut s = seed.max(1);
        (0..n)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s % 20_001) as f64 / 10_000.0 - 1.0
            })
            .collect()
    }

    fn series(base: f64, spread: f64, seed: u64, n: usize) -> Vec<f64> {
        noise(seed, n)
            .into_iter()
            .map(|e| base + spread * e)
            .collect()
    }

    #[test]
    fn normal_cdf_known_values() {
        assert!((normal_cdf(0.0) - 0.5).abs() < 1e-7);
        assert!((normal_cdf(1.959_964) - 0.975).abs() < 1e-6);
        assert!((normal_cdf(-1.644_854) - 0.05).abs() < 1e-6);
    }

    #[test]
    fn mann_whitney_matches_reference() {
        // Fully separated samples of 10 vs 10: exact one-sided p ~ 5.4e-6;
        // the normal approximation gives ~9e-5.
        let before: Vec<f64> = (0..10).map(f64::from).collect();
        let after: Vec<f64> = (10..20).map(f64::from).collect();
        let p = mann_whitney_greater(&before, &after);
        assert!(p < 1e-3, "{p}");
        assert!(mann_whitney_greater(&after, &before) > 0.999);
        // Identical samples: no evidence either way.
        assert_eq!(mann_whitney_greater(&[1.0; 8], &[1.0; 8]), 1.0);
        assert_eq!(mann_whitney_greater(&[], &[1.0]), 1.0);
    }

    #[test]
    fn block_means_and_median() {
        assert_eq!(
            block_means(&[1.0, 3.0, 5.0, 7.0, 9.0], 2),
            vec![2.0, 6.0, 9.0]
        );
        assert_eq!(block_means(&[1.0, 3.0, 5.0], 4), vec![3.0]);
        assert_eq!(block_means(&[1.0], 4), Vec::<f64>::new());
        assert_eq!(median(&[3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&[4.0, 1.0, 2.0, 3.0]), Some(2.5));
        assert_eq!(median(&[]), None);
    }

    #[test]
    fn known_latency_regression_is_flagged() {
        // P95 ~200 ms before, ~260 ms after (+30%), ±20 ms noise.
        let before = series(200.0, 20.0, 7, 24);
        let after = series(260.0, 20.0, 11, 24);
        let c = compare_continuous(&before, &after, TH);
        assert_eq!(c.verdict, Verdict::Regression, "{c:?}");
        assert!(c.relative_change.unwrap() > 0.2);
    }

    #[test]
    fn noise_alone_is_not_flagged() {
        // Many independent noise-only comparisons: none may be flagged at
        // alpha = 0.01 with a 10% minimum effect.
        for seed in 1..50 {
            let before = series(200.0, 30.0, seed, 24);
            let after = series(200.0, 30.0, seed * 7919, 24);
            let c = compare_continuous(&before, &after, TH);
            assert_eq!(
                c.verdict,
                Verdict::NoSignificantChange,
                "seed {seed}: {c:?}"
            );
        }
    }

    #[test]
    fn significant_but_tiny_change_is_not_flagged() {
        // 2% shift with almost no noise: significant, but below the 10%
        // minimum effect size.
        let before = series(100.0, 0.1, 3, 24);
        let after = series(102.0, 0.1, 5, 24);
        let c = compare_continuous(&before, &after, TH);
        assert!(c.p_value.unwrap() < TH.alpha);
        assert_eq!(c.verdict, Verdict::NoSignificantChange);

        let th = Thresholds {
            min_absolute_change: 5.0,
            min_relative_change: 0.0,
            ..TH
        };
        assert_eq!(
            compare_continuous(&before, &after, th).verdict,
            Verdict::NoSignificantChange
        );
    }

    #[test]
    fn improvement_and_insufficient_data() {
        let before = series(0.05, 0.005, 3, 24);
        let after = series(0.01, 0.005, 5, 24);
        assert_eq!(
            compare_continuous(&before, &after, TH).verdict,
            Verdict::Improvement
        );
        let c = compare_continuous(&before[..3], &after, TH);
        assert_eq!(c.verdict, Verdict::InsufficientData);
        assert!(c.p_value.is_none());
    }

    #[test]
    fn zero_baseline_relative_change() {
        let zeros = vec![0.0; 12];
        let c = compare_continuous(&zeros, &zeros, TH);
        assert_eq!(c.relative_change, Some(0.0));
        assert_eq!(c.verdict, Verdict::NoSignificantChange);
        let c = compare_continuous(&zeros, &[0.02; 12], TH);
        assert_eq!(c.relative_change, Some(f64::INFINITY));
        assert_eq!(c.verdict, Verdict::Regression);
    }

    #[test]
    fn binomial_tail_reference_values() {
        // P(X >= 8 | n = 10, p = 0.5) = 56/1024
        assert!((binomial_upper_tail(8, 10, 0.5) - 56.0 / 1024.0).abs() < 1e-12);
        assert_eq!(binomial_upper_tail(0, 10, 0.5), 1.0);
        assert_eq!(binomial_upper_tail(11, 10, 0.5), 0.0);
        assert_eq!(binomial_upper_tail(1, 10, 0.0), 0.0);
        assert_eq!(binomial_upper_tail(1, 10, 1.0), 1.0);
        // Large n stays finite.
        let p = binomial_upper_tail(600, 1000, 0.5);
        assert!(p > 0.0 && p < 1e-9, "{p}");
    }

    #[test]
    fn incident_counts() {
        // 1 incident before vs 12 after over equal windows: regression.
        let c = compare_counts(1, 24.0, 12, 24.0, 0.01);
        assert_eq!(c.verdict, Verdict::Regression);
        // 2 vs 3: noise.
        assert_eq!(
            compare_counts(2, 24.0, 3, 24.0, 0.01).verdict,
            Verdict::NoSignificantChange
        );
        // Same rate over unequal windows: noise.
        assert_eq!(
            compare_counts(2, 6.0, 8, 24.0, 0.01).verdict,
            Verdict::NoSignificantChange
        );
        // 10 before, 0 after: improvement.
        assert_eq!(
            compare_counts(10, 24.0, 0, 24.0, 0.01).verdict,
            Verdict::Improvement
        );
        // None at all.
        let c = compare_counts(0, 24.0, 0, 24.0, 0.01);
        assert_eq!(
            (c.verdict, c.relative_change),
            (Verdict::NoSignificantChange, Some(0.0))
        );
        assert_eq!(
            compare_counts(0, 0.0, 1, 24.0, 0.01).verdict,
            Verdict::InsufficientData
        );
        assert_eq!(
            compare_counts(0, 24.0, 5, 24.0, 0.01).relative_change,
            Some(f64::INFINITY)
        );
    }

    #[test]
    fn verdict_labels() {
        for v in [
            Verdict::Regression,
            Verdict::Improvement,
            Verdict::NoSignificantChange,
            Verdict::InsufficientData,
        ] {
            assert!(!v.label().is_empty());
        }
    }
}
