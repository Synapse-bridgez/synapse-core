//! Fixed-bucket histogram helpers shared by the dependency scorecard
//! (`services::dependency_scorecard`) and per-tenant latency histograms
//! (`tenant::latency`).
//!
//! Both store latency as counts in a small, reviewed set of buckets rather
//! than raw samples, so histograms from different instances / time periods
//! can be merged by element-wise addition and percentiles estimated from the
//! merged counts.

/// Index of the bucket `value` falls into, given ascending finite upper
/// bounds (`le` semantics: a value equal to a bound belongs to that bound's
/// bucket). Values above the last bound land in the overflow bucket at
/// index `bounds.len()`, so a histogram over `bounds` has `bounds.len() + 1`
/// buckets.
pub fn bucket_index(bounds: &[f64], value: f64) -> usize {
    bounds
        .iter()
        .position(|&upper| value <= upper)
        .unwrap_or(bounds.len())
}

/// Adds `src` into `dst` element-wise. Extra trailing elements in `src`
/// (e.g. a row persisted with a different bucket layout) are ignored rather
/// than panicking.
pub fn merge_into(dst: &mut [u64], src: &[u64]) {
    for (d, s) in dst.iter_mut().zip(src.iter()) {
        *d = d.saturating_add(*s);
    }
}

/// Estimates quantile `q` (0.0..=1.0) from non-cumulative bucket counts,
/// interpolating linearly inside the bucket the quantile lands in — the same
/// estimate Prometheus' `histogram_quantile` makes. The first bucket's lower
/// bound is 0. A quantile that lands in the overflow bucket is reported as
/// the last finite bound (there is no upper edge to interpolate towards);
/// callers should read that as "at least this much".
///
/// Returns `None` for an empty histogram.
pub fn bucket_quantile(bounds: &[f64], counts: &[u64], q: f64) -> Option<f64> {
    let total: u64 = counts.iter().sum();
    if total == 0 {
        return None;
    }
    let q = q.clamp(0.0, 1.0);
    let rank = q * total as f64;
    let mut cumulative = 0u64;
    for (i, &count) in counts.iter().enumerate() {
        if count == 0 {
            continue;
        }
        let next = cumulative + count;
        if (next as f64) >= rank {
            if i >= bounds.len() {
                return bounds.last().copied();
            }
            let lower = if i == 0 { 0.0 } else { bounds[i - 1] };
            let upper = bounds[i];
            let within = (rank - cumulative as f64) / count as f64;
            return Some(lower + (upper - lower) * within.clamp(0.0, 1.0));
        }
        cumulative = next;
    }
    bounds.last().copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUNDS: [f64; 3] = [10.0, 100.0, 1000.0];

    #[test]
    fn bucket_index_uses_le_semantics() {
        assert_eq!(bucket_index(&BOUNDS, 0.0), 0);
        assert_eq!(bucket_index(&BOUNDS, 10.0), 0);
        assert_eq!(bucket_index(&BOUNDS, 10.01), 1);
        assert_eq!(bucket_index(&BOUNDS, 1000.0), 2);
        assert_eq!(bucket_index(&BOUNDS, 1000.5), 3);
    }

    #[test]
    fn merge_adds_elementwise_and_tolerates_length_mismatch() {
        let mut dst = vec![1, 2, 3, 4];
        merge_into(&mut dst, &[1, 1, 1, 1, 99]);
        assert_eq!(dst, vec![2, 3, 4, 5]);
        merge_into(&mut dst, &[1]);
        assert_eq!(dst, vec![3, 3, 4, 5]);
    }

    #[test]
    fn quantile_of_empty_histogram_is_none() {
        assert_eq!(bucket_quantile(&BOUNDS, &[0, 0, 0, 0], 0.5), None);
    }

    #[test]
    fn quantile_interpolates_within_bucket() {
        // 100 samples, all in (10, 100].
        let counts = [0, 100, 0, 0];
        let p50 = bucket_quantile(&BOUNDS, &counts, 0.5).unwrap();
        assert!((p50 - 55.0).abs() < 1e-9, "p50 = {p50}");
        let p100 = bucket_quantile(&BOUNDS, &counts, 1.0).unwrap();
        assert!((p100 - 100.0).abs() < 1e-9);
    }

    #[test]
    fn quantile_in_overflow_bucket_reports_last_bound() {
        let counts = [1, 0, 0, 99];
        assert_eq!(bucket_quantile(&BOUNDS, &counts, 0.99), Some(1000.0));
    }

    #[test]
    fn quantile_skips_empty_leading_buckets() {
        let counts = [0, 0, 10, 0];
        let p0 = bucket_quantile(&BOUNDS, &counts, 0.0).unwrap();
        assert!((100.0..=1000.0).contains(&p0));
    }
}
