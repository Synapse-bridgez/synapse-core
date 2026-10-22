//! Execution slicing algorithms for large swap orders.
//!
//! This module defines the [`ExecutionAlgorithm`] contract and ships three
//! reference implementations:
//!
//! * [`twap::TwapAlgorithm`] — time-weighted slicing.
//! * [`vwap::VwapAlgorithm`] — volume-profile weighted slicing.
//! * [`iceberg::IcebergAlgorithm`] — capped visible-size slicing.
//!
//! An algorithm is driven by the execution engine: given the parent order
//! state and a market snapshot it returns the next [`SlicePlan`] to submit, or
//! `None` when the parent order is complete (or the window has expired and the
//! terminal behaviour has been resolved).

pub mod iceberg;
pub mod twap;
pub mod vwap;

use std::fmt;

/// Terminal behaviour for the unfilled remainder once the execution window
/// expires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalBehavior {
    /// Submit the remaining quantity as a single market order.
    Market,
    /// Cancel the parent order, leaving the remainder unfilled.
    Cancel,
    /// Extend the window by one additional interval and keep working.
    Extend,
}

/// Urgency hint that algorithms may use to bias slice sizing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    Low,
    Normal,
    High,
}

/// Per-order configuration for an execution algorithm.
#[derive(Debug, Clone)]
pub struct ExecutionConfig {
    /// Total parent quantity to work into the market.
    pub parent_quantity: f64,
    /// Total execution window in seconds.
    pub window_secs: u64,
    /// Number of slices to schedule across the window. When `None` the
    /// algorithm derives it from `interval_secs`.
    pub slice_count: Option<u32>,
    /// Interval between slices in seconds. Used when `slice_count` is `None`.
    pub interval_secs: Option<u64>,
    /// Minimum size of a single slice.
    pub min_slice_size: f64,
    /// Maximum size of a single slice.
    pub max_slice_size: f64,
    /// Randomization band applied to slice sizes, expressed as a fraction of
    /// the nominal slice size (e.g. `0.1` for ±10%).
    pub randomization_band: f64,
    /// Urgency hint.
    pub urgency: Urgency,
    /// Behaviour for the unfilled remainder at window expiry.
    pub terminal_behavior: TerminalBehavior,
    /// Deterministic RNG seed for reproducible slice sizing.
    pub rng_seed: u64,
}

impl ExecutionConfig {
    /// Resolve the effective number of slices for the window.
    pub fn effective_slice_count(&self) -> u32 {
        if let Some(count) = self.slice_count {
            return count.max(1);
        }
        if let Some(interval) = self.interval_secs {
            if interval > 0 {
                return ((self.window_secs / interval).max(1)) as u32;
            }
        }
        1
    }

    /// Resolve the effective interval between slices, in seconds.
    pub fn effective_interval_secs(&self) -> u64 {
        if let Some(interval) = self.interval_secs {
            if interval > 0 {
                return interval;
            }
        }
        (self.window_secs / self.effective_slice_count() as u64).max(1)
    }
}

/// A single child-order slice produced by an algorithm.
#[derive(Debug, Clone, PartialEq)]
pub struct SlicePlan {
    /// Quantity to submit for this slice.
    pub size: f64,
    /// Delay in seconds from now before the slice should be submitted.
    pub delay_secs: u64,
    /// Optional cap on the visible size (used by iceberg).
    pub visible_size_cap: Option<f64>,
}

/// Mutable per-parent-order state tracked by the engine across slices.
#[derive(Debug, Clone)]
pub struct ParentOrderState {
    /// Total quantity already filled across child orders.
    pub filled_quantity: f64,
    /// Number of slices already submitted.
    pub slices_submitted: u32,
    /// Seconds elapsed since the parent order started.
    pub elapsed_secs: u64,
}

impl ParentOrderState {
    pub fn new() -> Self {
        Self {
            filled_quantity: 0.0,
            slices_submitted: 0,
            elapsed_secs: 0,
        }
    }

    /// Remaining quantity still to be worked.
    pub fn remaining(&self, parent_quantity: f64) -> f64 {
        (parent_quantity - self.filled_quantity).max(0.0)
    }
}

impl Default for ParentOrderState {
    fn default() -> Self {
        Self::new()
    }
}

/// A point-in-time market snapshot supplied to algorithms.
#[derive(Debug, Clone)]
pub struct MarketSnapshot {
    /// Estimated available liquidity for the pair.
    pub available_liquidity: f64,
    /// Volume profile buckets (fraction of expected volume per interval).
    /// Empty when no profile is available (e.g. stale data).
    pub volume_profile: Vec<f64>,
    /// Whether the snapshot is considered stale.
    pub stale: bool,
}

impl MarketSnapshot {
    pub fn empty() -> Self {
        Self {
            available_liquidity: 0.0,
            volume_profile: Vec::new(),
            stale: true,
        }
    }
}

/// Errors surfaced by execution algorithms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionError {
    /// The configuration is invalid (e.g. min > max slice size).
    InvalidConfig(String),
    /// The parent quantity has already been fully filled.
    Complete,
}

impl fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExecutionError::InvalidConfig(msg) => write!(f, "invalid execution config: {msg}"),
            ExecutionError::Complete => write!(f, "parent order already complete"),
        }
    }
}

impl std::error::Error for ExecutionError {}

/// Contract implemented by every execution algorithm.
pub trait ExecutionAlgorithm: Send + Sync {
    /// Human-readable algorithm name (e.g. `"twap"`).
    fn name(&self) -> &'static str;

    /// Produce the next slice to submit, or `None` when the parent order is
    /// complete or the window has expired and the terminal behaviour has been
    /// resolved.
    fn next_slice(
        &self,
        config: &ExecutionConfig,
        state: &ParentOrderState,
        market: &MarketSnapshot,
    ) -> Result<Option<SlicePlan>, ExecutionError>;
}

/// Clamp a nominal slice size to the configured min/max bounds and the
/// remaining parent quantity.
pub(crate) fn clamp_slice(
    nominal: f64,
    config: &ExecutionConfig,
    remaining: f64,
) -> f64 {
    let mut size = nominal.max(config.min_slice_size);
    if config.max_slice_size > 0.0 {
        size = size.min(config.max_slice_size);
    }
    size.min(remaining).max(0.0)
}

/// Deterministic pseudo-random factor in `[1 - band, 1 + band]` derived from
/// the seed and slice index, so tests are reproducible.
pub(crate) fn randomization_factor(seed: u64, index: u32, band: f64) -> f64 {
    if band <= 0.0 {
        return 1.0;
    }
    // Simple xorshift-style mix; deterministic and dependency-free.
    let mut x = seed ^ ((index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    let unit = (x % 10_000) as f64 / 10_000.0; // [0, 1)
    let band = band.clamp(0.0, 1.0);
    1.0 - band + unit * 2.0 * band
}

/// Validate common configuration invariants shared by all algorithms.
pub(crate) fn validate_config(config: &ExecutionConfig) -> Result<(), ExecutionError> {
    if config.parent_quantity <= 0.0 {
        return Err(ExecutionError::InvalidConfig(
            "parent_quantity must be positive".into(),
        ));
    }
    if config.window_secs == 0 {
        return Err(ExecutionError::InvalidConfig(
            "window_secs must be positive".into(),
        ));
    }
    if config.min_slice_size < 0.0 {
        return Err(ExecutionError::InvalidConfig(
            "min_slice_size must be non-negative".into(),
        ));
    }
    if config.max_slice_size > 0.0 && config.max_slice_size < config.min_slice_size {
        return Err(ExecutionError::InvalidConfig(
            "max_slice_size must be >= min_slice_size".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_config() -> ExecutionConfig {
        ExecutionConfig {
            parent_quantity: 1000.0,
            window_secs: 600,
            slice_count: Some(10),
            interval_secs: None,
            min_slice_size: 1.0,
            max_slice_size: 200.0,
            randomization_band: 0.0,
            urgency: Urgency::Normal,
            terminal_behavior: TerminalBehavior::Cancel,
            rng_seed: 42,
        }
    }

    #[test]
    fn effective_slice_count_from_interval() {
        let mut cfg = base_config();
        cfg.slice_count = None;
        cfg.interval_secs = Some(60);
        assert_eq!(cfg.effective_slice_count(), 10);
        assert_eq!(cfg.effective_interval_secs(), 60);
    }

    #[test]
    fn clamp_respects_bounds_and_remaining() {
        let cfg = base_config();
        assert_eq!(clamp_slice(500.0, &cfg, 1000.0), 200.0);
        assert_eq!(clamp_slice(0.1, &cfg, 1000.0), 1.0);
        assert_eq!(clamp_slice(150.0, &cfg, 50.0), 50.0);
    }

    #[test]
    fn randomization_is_deterministic_and_bounded() {
        let a = randomization_factor(7, 3, 0.2);
        let b = randomization_factor(7, 3, 0.2);
        assert_eq!(a, b);
        assert!((0.8..=1.2).contains(&a));
    }

    #[test]
    fn validate_rejects_bad_bounds() {
        let mut cfg = base_config();
        cfg.min_slice_size = 300.0;
        assert!(validate_config(&cfg).is_err());
    }
}
