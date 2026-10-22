//! Swap order execution engine.
//!
//! Provides the execution-algorithm layer that slices a large parent swap
//! order into child orders scheduled over a configurable window. Child orders
//! are submitted through the same order submission path as normal orders so
//! idempotency, risk/compliance checks, and slippage guards all apply.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

/// Terminal behavior for the unfilled remainder when the execution window
/// expires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalBehavior {
    /// Fire the remaining quantity as a single market order.
    Market,
    /// Cancel the remainder and leave the parent partially filled.
    Cancel,
    /// Extend the window and keep working the remainder.
    Extend,
}

/// Urgency influences slice sizing (higher urgency front-loads volume).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Urgency {
    Low,
    Normal,
    High,
}

impl Urgency {
    /// Multiplier applied to the base slice size.
    fn multiplier(self) -> f64 {
        match self {
            Urgency::Low => 0.75,
            Urgency::Normal => 1.0,
            Urgency::High => 1.5,
        }
    }
}

/// Per-order execution-algorithm configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionConfig {
    /// Algorithm identifier: `twap`, `vwap`, or `iceberg`.
    pub algorithm: String,
    /// Total execution window in seconds.
    pub window_secs: u64,
    /// Number of slices to schedule across the window.
    pub slice_count: u32,
    /// Minimum size of any single slice.
    pub min_slice_size: f64,
    /// Maximum size of any single slice.
    pub max_slice_size: f64,
    /// Fractional randomization band applied to slice sizes (0.0..=1.0).
    pub randomization_band: f64,
    /// Execution urgency.
    pub urgency: Urgency,
    /// Terminal behavior for the unfilled remainder.
    pub terminal_behavior: TerminalBehavior,
    /// Optional visible-size cap (iceberg).
    pub visible_size_cap: Option<f64>,
    /// Deterministic RNG seed for reproducible slice sizing.
    pub rng_seed: u64,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            algorithm: "twap".to_string(),
            window_secs: 3600,
            slice_count: 10,
            min_slice_size: 0.0,
            max_slice_size: f64::INFINITY,
            randomization_band: 0.0,
            urgency: Urgency::Normal,
            terminal_behavior: TerminalBehavior::Cancel,
            visible_size_cap: None,
            rng_seed: 0,
        }
    }
}

/// A market snapshot supplied to the algorithm when computing the next slice.
#[derive(Debug, Clone, Default)]
pub struct MarketSnapshot {
    /// Recent traded volume used by VWAP volume-profile weighting.
    pub recent_volume: f64,
    /// Estimated available liquidity for the pair.
    pub available_liquidity: f64,
    /// Whether the snapshot is considered stale.
    pub stale: bool,
}

/// Parent-order state visible to the algorithm.
#[derive(Debug, Clone)]
pub struct ParentOrderState {
    /// Total parent quantity to work.
    pub quantity: f64,
    /// Quantity already filled across child orders.
    pub filled: f64,
    /// Number of slices already submitted.
    pub slices_submitted: u32,
    /// Seconds elapsed since the parent order started.
    pub elapsed_secs: u64,
}

impl ParentOrderState {
    /// Remaining quantity still to be worked.
    pub fn remaining(&self) -> f64 {
        (self.quantity - self.filled).max(0.0)
    }
}

/// A single child-order slice produced by an execution algorithm.
#[derive(Debug, Clone, PartialEq)]
pub struct SlicePlan {
    /// Size of the child order.
    pub size: f64,
    /// Delay in seconds before this slice should be submitted.
    pub delay_secs: u64,
    /// Optional visible-size cap for iceberg-style execution.
    pub visible_size_cap: Option<f64>,
}

/// Contract implemented by every execution algorithm.
///
/// Given the parent-order state and a market snapshot, returns the next
/// [`SlicePlan`] to submit, or `None` when the parent order is complete or the
/// window has expired.
pub trait ExecutionAlgorithm {
    /// Compute the next slice, or `None` if nothing remains to schedule.
    fn next_slice(
        &self,
        config: &ExecutionConfig,
        state: &ParentOrderState,
        market: &MarketSnapshot,
    ) -> Option<SlicePlan>;
}

/// Deterministic pseudo-random value in `[-1.0, 1.0]` derived from a seed and
/// slice index, so tests are reproducible without an RNG dependency.
fn deterministic_jitter(seed: u64, index: u32) -> f64 {
    let mut x = seed.wrapping_add(index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    let unit = (x >> 11) as f64 / (1u64 << 53) as f64; // [0, 1)
    unit * 2.0 - 1.0
}

/// Clamp a raw slice size to the configured min/max caps and the remaining
/// parent quantity.
fn clamp_slice(raw: f64, config: &ExecutionConfig, remaining: f64) -> f64 {
    let mut size = raw.max(config.min_slice_size);
    if config.max_slice_size.is_finite() {
        size = size.min(config.max_slice_size);
    }
    size.min(remaining).max(0.0)
}

/// Apply the randomization band to a base slice size.
fn apply_band(base: f64, config: &ExecutionConfig, index: u32) -> f64 {
    if config.randomization_band <= 0.0 {
        return base;
    }
    let band = config.randomization_band.clamp(0.0, 1.0);
    let jitter = deterministic_jitter(config.rng_seed, index);
    base * (1.0 + band * jitter)
}

/// Time-weighted average price: equal-sized slices spread evenly across the
/// window.
pub struct Twap;

impl ExecutionAlgorithm for Twap {
    fn next_slice(
        &self,
        config: &ExecutionConfig,
        state: &ParentOrderState,
        _market: &MarketSnapshot,
    ) -> Option<SlicePlan> {
        let remaining = state.remaining();
        if remaining <= 0.0 || state.elapsed_secs >= config.window_secs {
            return None;
        }
        let slices_left = config.slice_count.saturating_sub(state.slices_submitted).max(1);
        let base = (remaining / slices_left as f64) * config.urgency.multiplier();
        let sized = apply_band(base, config, state.slices_submitted);
        let size = clamp_slice(sized, config, remaining);
        if size <= 0.0 {
            return None;
        }
        let interval = config.window_secs / config.slice_count.max(1) as u64;
        Some(SlicePlan {
            size,
            delay_secs: interval,
            visible_size_cap: config.visible_size_cap,
        })
    }
}

/// Volume-profile weighted average price: slice sizes proportional to recent
/// traded volume, falling back to TWAP sizing when the snapshot is stale or
/// volume is unavailable.
pub struct Vwap;

impl ExecutionAlgorithm for Vwap {
    fn next_slice(
        &self,
        config: &ExecutionConfig,
        state: &ParentOrderState,
        market: &MarketSnapshot,
    ) -> Option<SlicePlan> {
        let remaining = state.remaining();
        if remaining <= 0.0 || state.elapsed_secs >= config.window_secs {
            return None;
        }
        if market.stale || market.recent_volume <= 0.0 {
            return Twap.next_slice(config, state, market);
        }
        let slices_left = config.slice_count.saturating_sub(state.slices_submitted).max(1);
        // Weight by volume relative to the average slice volume.
        let avg_volume = market.recent_volume / config.slice_count.max(1) as f64;
        let weight = (market.recent_volume / avg_volume).max(0.0);
        let base = (remaining / slices_left as f64) * weight * config.urgency.multiplier();
        let sized = apply_band(base, config, state.slices_submitted);
        let size = clamp_slice(sized, config, remaining);
        if size <= 0.0 {
            return None;
        }
        let interval = config.window_secs / config.slice_count.max(1) as u64;
        Some(SlicePlan {
            size,
            delay_secs: interval,
            visible_size_cap: config.visible_size_cap,
        })
    }
}

/// Iceberg: works the parent order in slices whose visible size is capped,
/// hiding the true order size from the market.
pub struct Iceberg;

impl ExecutionAlgorithm for Iceberg {
    fn next_slice(
        &self,
        config: &ExecutionConfig,
        state: &ParentOrderState,
        _market: &MarketSnapshot,
    ) -> Option<SlicePlan> {
        let remaining = state.remaining();
        if remaining <= 0.0 || state.elapsed_secs >= config.window_secs {
            return None;
        }
        let cap = config.visible_size_cap.unwrap_or(config.max_slice_size);
        let slices_left = config.slice_count.saturating_sub(state.slices_submitted).max(1);
        let base = (remaining / slices_left as f64) * config.urgency.multiplier();
        let sized = apply_band(base, config, state.slices_submitted);
        let size = clamp_slice(sized, config, remaining);
        if size <= 0.0 {
            return None;
        }
        let interval = config.window_secs / config.slice_count.max(1) as u64;
        Some(SlicePlan {
            size,
            delay_secs: interval,
            visible_size_cap: Some(cap.min(size)),
        })
    }
}

/// Resolve an algorithm by its configured identifier.
pub fn algorithm_for(name: &str) -> Option<Box<dyn ExecutionAlgorithm + Send + Sync>> {
    match name {
        "twap" => Some(Box::new(Twap)),
        "vwap" => Some(Box::new(Vwap)),
        "iceberg" => Some(Box::new(Iceberg)),
        _ => None,
    }
}

/// Persisted slice plan and per-slice progress so an in-progress parent order
/// survives a restart and resumes correctly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SliceProgress {
    /// Index of the slice within the plan.
    pub index: u32,
    /// Planned size of the slice.
    pub size: f64,
    /// Whether the slice has been submitted.
    pub submitted: bool,
    /// Quantity filled for this slice.
    pub filled: f64,
}

/// Persisted execution state for a parent order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionState {
    /// Parent order identifier.
    pub parent_order_id: String,
    /// Configuration used for this parent order.
    pub config: ExecutionConfig,
    /// Planned slices and their progress.
    pub slices: Vec<SliceProgress>,
    /// Seconds elapsed since the parent order started.
    pub elapsed_secs: u64,
}

impl ExecutionState {
    /// Build a fresh execution state for a parent order.
    pub fn new(parent_order_id: impl Into<String>, config: ExecutionConfig) -> Self {
        Self {
            parent_order_id: parent_order_id.into(),
            config,
            slices: Vec::new(),
            elapsed_secs: 0,
        }
    }

    /// Total quantity filled across all slices.
    pub fn total_filled(&self) -> f64 {
        self.slices.iter().map(|s| s.filled).sum()
    }

    /// Number of slices already submitted.
    pub fn slices_submitted(&self) -> u32 {
        self.slices.iter().filter(|s| s.submitted).count() as u32
    }

    /// Compute the next slice for this parent order, recording it in the plan.
    pub fn next_slice(
        &mut self,
        algorithm: &dyn ExecutionAlgorithm,
        quantity: f64,
        market: &MarketSnapshot,
    ) -> Option<SlicePlan> {
        let state = ParentOrderState {
            quantity,
            filled: self.total_filled(),
            slices_submitted: self.slices_submitted(),
            elapsed_secs: self.elapsed_secs,
        };
        let plan = algorithm.next_slice(&self.config, &state, market)?;
        self.slices.push(SliceProgress {
            index: self.slices.len() as u32,
            size: plan.size,
            submitted: false,
            filled: 0.0,
        });
        Some(plan)
    }

    /// Mark a slice as submitted (idempotent) and advance the clock.
    pub fn mark_submitted(&mut self, index: u32, delay_secs: u64) {
        if let Some(slice) = self.slices.get_mut(index as usize) {
            slice.submitted = true;
        }
        self.elapsed_secs = self.elapsed_secs.saturating_add(delay_secs);
    }

    /// Record a fill for a slice, aggregating into the parent order.
    pub fn record_fill(&mut self, index: u32, filled: f64) {
        if let Some(slice) = self.slices.get_mut(index as usize) {
            slice.filled = (slice.filled + filled).min(slice.size);
        }
    }

    /// Determine the terminal action for any unfilled remainder once the
    /// window has expired.
    pub fn terminal_action(&self, quantity: f64) -> Option<TerminalBehavior> {
        if self.elapsed_secs >= self.config.window_secs && self.total_filled() < quantity {
            Some(self.config.terminal_behavior)
        } else {
            None
        }
    }
}

/// Driver loop that works a parent order off the existing job/worker
/// mechanism, inheriting backpressure and graceful shutdown.
pub struct ExecutionDriver {
    state: ExecutionState,
    quantity: f64,
    algorithm: Box<dyn ExecutionAlgorithm + Send + Sync>,
    pending: VecDeque<SlicePlan>,
}

impl ExecutionDriver {
    /// Create a driver for a parent order, resuming from persisted state.
    pub fn new(
        state: ExecutionState,
        quantity: f64,
        algorithm: Box<dyn ExecutionAlgorithm + Send + Sync>,
    ) -> Self {
        Self {
            state,
            quantity,
            algorithm,
            pending: VecDeque::new(),
        }
    }

    /// Access the persisted execution state (for checkpointing).
    pub fn state(&self) -> &ExecutionState {
        &self.state
    }

    /// Produce the next slice to submit, or `None` when the parent order is
    /// complete or the window has expired.
    pub fn next(&mut self, market: &MarketSnapshot) -> Option<SlicePlan> {
        if let Some(plan) = self.pending.pop_front() {
            return Some(plan);
        }
        self.state.next_slice(self.algorithm.as_ref(), self.quantity, market)
    }

    /// Record that a slice was submitted and advance the execution clock.
    pub fn on_submitted(&mut self, index: u32, delay_secs: u64) {
        self.state.mark_submitted(index, delay_secs);
    }

    /// Record a child-order fill, aggregating into the parent order.
    pub fn on_fill(&mut self, index: u32, filled: f64) {
        self.state.record_fill(index, filled);
    }
}
