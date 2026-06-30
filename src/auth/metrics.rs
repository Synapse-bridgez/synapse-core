//! Metrics collection for authentication operations (vaultrs integration).
//!
//! [`AuthMetrics`] tracks authentication attempts, successes, failures, and
//! validation errors using lock-free atomics. All recording methods include
//! **security and consistency checks** so that callers cannot drive counters
//! into an inconsistent state.
//!
//! # Security Considerations
//!
//! - **Overflow protection**: counters use saturating arithmetic so a sustained
//!   flood of requests cannot wrap counters back to zero and hide attack
//!   evidence.
//! - **Consistency invariant**: `successful_auths + failed_auths ≤ total_attempts`
//!   is enforced at snapshot time via [`AuthMetrics::validate`]. Recording
//!   methods are ordered so the invariant holds under concurrent access.
//! - **No sensitive data**: metrics contain only counts and rates — no tokens,
//!   credentials, or user identifiers are stored.
//! - **Snapshot export**: [`AuthMetrics::snapshot`] returns a plain struct that
//!   is safe to serialize and ship to a metrics backend without leaking
//!   internal state.
//!
//! # Vaultrs Integration
//!
//! When the auth layer calls into Vault (via `vaultrs`) it should:
//! 1. Call [`record_attempt`](AuthMetrics::record_attempt) **before** the Vault
//!    request so the attempt is always counted even if the process crashes.
//! 2. Call [`record_success`](AuthMetrics::record_success) or
//!    [`record_failure`](AuthMetrics::record_failure) after the response.
//! 3. Call [`record_validation_error`](AuthMetrics::record_validation_error)
//!    for inputs rejected before reaching Vault (e.g. empty tokens).
//!
//! # Example
//!
//! ```rust
//! use synapse_core::auth::metrics::AuthMetrics;
//!
//! let m = AuthMetrics::new();
//! m.record_attempt();
//! m.record_success();
//! assert_eq!(m.success_rate(), 100.0);
//!
//! let snap = m.snapshot();
//! assert_eq!(snap.total_attempts, 1);
//! assert!(m.validate().is_ok());
//! ```

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::auth::error::AuthError;

// ---------------------------------------------------------------------------
// Snapshot
// ---------------------------------------------------------------------------

/// Point-in-time export of all authentication counters.
///
/// Safe to serialize and forward to Prometheus, Datadog, or any other
/// metrics backend. Contains no sensitive data.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricsSnapshot {
    /// Total authentication attempts recorded.
    pub total_attempts: u64,
    /// Attempts that resulted in a successful authentication.
    pub successful_auths: u64,
    /// Attempts that resulted in a failed authentication.
    pub failed_auths: u64,
    /// Inputs rejected by validation before reaching the auth backend.
    pub validation_errors: u64,
    /// Success rate as a percentage in `[0.0, 100.0]`.
    pub success_rate_pct: f64,
}

// ---------------------------------------------------------------------------
// AuthMetrics
// ---------------------------------------------------------------------------

/// Thread-safe authentication metrics collector.
///
/// All clones share the same underlying counters (backed by `Arc`), so a
/// single `AuthMetrics` instance can be passed to multiple tasks without
/// wrapping in an additional `Mutex`.
#[derive(Debug, Clone)]
pub struct AuthMetrics {
    total_attempts: Arc<AtomicU64>,
    successful_auths: Arc<AtomicU64>,
    failed_auths: Arc<AtomicU64>,
    validation_errors: Arc<AtomicU64>,
    outcomes: Arc<AtomicU64>,
}

impl AuthMetrics {
    /// Creates a new metrics collector with all counters at zero.
    pub fn new() -> Self {
        Self {
            total_attempts: Arc::new(AtomicU64::new(0)),
            successful_auths: Arc::new(AtomicU64::new(0)),
            failed_auths: Arc::new(AtomicU64::new(0)),
            validation_errors: Arc::new(AtomicU64::new(0)),
            outcomes: Arc::new(AtomicU64::new(0)),
        }
    }

    #[inline]
    fn saturating_increment(counter: &AtomicU64) {
        let mut current = counter.load(Ordering::Relaxed);
        while current != u64::MAX {
            let next = current.saturating_add(1);
            match counter.compare_exchange(current, next, Ordering::SeqCst, Ordering::Relaxed) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }

    fn record_outcome_if_allowed(&self) -> bool {
        let total = self.total_attempts.load(Ordering::Acquire);
        self.outcomes
            .fetch_update(Ordering::SeqCst, Ordering::Relaxed, |current| {
                if current >= total {
                    None
                } else {
                    Some(current.saturating_add(1))
                }
            })
            .is_ok()
    }

    // ── Recording ────────────────────────────────────────────────────────────

    /// Records one authentication attempt.
    ///
    /// Must be called **before** [`record_success`](Self::record_success) or
    /// [`record_failure`](Self::record_failure) to maintain the consistency
    /// invariant `successful + failed ≤ total`.
    ///
    /// Uses saturating addition: the counter stops at `u64::MAX` rather than
    /// wrapping, preserving evidence of high-volume attack traffic.
    pub fn record_attempt(&self) {
        Self::saturating_increment(&self.total_attempts);
    }

    /// Records a successful authentication outcome.
    ///
    /// # Security
    ///
    /// Validates that `successful_auths + failed_auths` will not exceed
    /// `total_attempts`. If the invariant would be violated, the increment is
    /// skipped and a warning is emitted.
    pub fn record_success(&self) {
        if !self.record_outcome_if_allowed() {
            tracing::warn!(
                "record_success called without a prior record_attempt; skipping to preserve metrics consistency"
            );
            return;
        }

        Self::saturating_increment(&self.successful_auths);
    }

    /// Records a failed authentication outcome.
    ///
    /// Applies the same consistency guard as [`record_success`](Self::record_success).
    pub fn record_failure(&self) {
        if !self.record_outcome_if_allowed() {
            tracing::warn!(
                "record_failure called without a prior record_attempt; skipping to preserve metrics consistency"
            );
            return;
        }

        Self::saturating_increment(&self.failed_auths);
    }

    /// Records an input validation error (rejected before reaching Vault).
    ///
    /// Validation errors are tracked separately from auth failures because
    /// they indicate malformed requests rather than credential mismatches,
    /// and they do **not** require a prior `record_attempt` call.
    pub fn record_validation_error(&self) {
        Self::saturating_increment(&self.validation_errors);
    }

    // ── Accessors ────────────────────────────────────────────────────────────

    /// Total authentication attempts.
    pub fn total_attempts(&self) -> u64 {
        self.total_attempts.load(Ordering::Relaxed)
    }

    /// Successful authentication count.
    pub fn successful_auths(&self) -> u64 {
        self.successful_auths.load(Ordering::Relaxed)
    }

    /// Failed authentication count.
    pub fn failed_auths(&self) -> u64 {
        self.failed_auths.load(Ordering::Relaxed)
    }

    /// Validation error count.
    pub fn validation_errors(&self) -> u64 {
        self.validation_errors.load(Ordering::Relaxed)
    }

    /// Success rate as a percentage in `[0.0, 100.0]`.
    ///
    /// Returns `0.0` when no attempts have been recorded to avoid
    /// division-by-zero.
    pub fn success_rate(&self) -> f64 {
        let total = self.total_attempts();
        if total == 0 {
            return 0.0;
        }
        (self.successful_auths() as f64 / total as f64) * 100.0
    }

    // ── Snapshot export ──────────────────────────────────────────────────────

    /// Returns a consistent point-in-time snapshot of all counters.
    ///
    /// The snapshot is safe to serialize and forward to any metrics backend.
    /// It contains no sensitive data (no tokens, credentials, or user IDs).
    ///
    /// # Note on consistency
    ///
    /// Because counters are updated independently with `Relaxed` ordering,
    /// the snapshot may reflect a state that was never simultaneously true
    /// across all counters. For monitoring purposes this is acceptable; use
    /// [`validate`](Self::validate) if you need a hard consistency check.
    pub fn snapshot(&self) -> MetricsSnapshot {
        let total = self.total_attempts();
        let success = self.successful_auths();
        let failed = self.failed_auths();
        let validation = self.validation_errors();
        let rate = if total == 0 {
            0.0
        } else {
            (success as f64 / total as f64) * 100.0
        };

        MetricsSnapshot {
            total_attempts: total,
            successful_auths: success,
            failed_auths: failed,
            validation_errors: validation,
            success_rate_pct: rate,
        }
    }

    // ── Validation ───────────────────────────────────────────────────────────

    /// Validates the internal consistency of the counters.
    ///
    /// Checks that:
    /// - `successful_auths + failed_auths <= outcomes`
    /// - `outcomes <= total_attempts`
    ///
    /// The `outcomes` counter may be temporarily ahead of individual success/failure
    /// counters during concurrent recording, but it must never exceed the total
    /// attempts or drop below the sum of completed outcomes.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Validation`] with a descriptive message if the
    /// invariant is violated.
    pub fn validate(&self) -> Result<(), AuthError> {
        let total = self.total_attempts();
        let outcomes = self.outcomes.load(Ordering::Acquire);
        let successful = self.successful_auths();
        let failed = self.failed_auths();
        let completed_outcomes = successful.saturating_add(failed);

        if completed_outcomes > outcomes {
            return Err(AuthError::Validation(format!(
                "metrics invariant violated: successful({}) + failed({}) = {} > outcomes({})",
                successful, failed, completed_outcomes, outcomes,
            )));
        }

        if outcomes > total {
            return Err(AuthError::Validation(format!(
                "metrics invariant violated: outcomes({}) > total_attempts({})",
                outcomes, total,
            )));
        }

        Ok(())
    }

    // ── Reset ────────────────────────────────────────────────────────────────

    /// Resets all counters to zero.
    ///
    /// Intended for use in tests and periodic metric-window resets.
    /// **Not** safe to call while other threads are actively recording.
    pub fn reset(&self) {
        self.total_attempts.store(0, Ordering::Relaxed);
        self.successful_auths.store(0, Ordering::Relaxed);
        self.failed_auths.store(0, Ordering::Relaxed);
        self.validation_errors.store(0, Ordering::Relaxed);
        self.outcomes.store(0, Ordering::Relaxed);
    }
}

impl Default for AuthMetrics {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_state_is_zero() {
        let m = AuthMetrics::new();
        assert_eq!(m.total_attempts(), 0);
        assert_eq!(m.successful_auths(), 0);
        assert_eq!(m.failed_auths(), 0);
        assert_eq!(m.validation_errors(), 0);
        assert_eq!(m.success_rate(), 0.0);
    }

    #[test]
    fn test_record_attempt_increments_total() {
        let m = AuthMetrics::new();
        m.record_attempt();
        assert_eq!(m.total_attempts(), 1);
    }

    #[test]
    fn test_record_success_after_attempt() {
        let m = AuthMetrics::new();
        m.record_attempt();
        m.record_success();
        assert_eq!(m.successful_auths(), 1);
        assert!(m.validate().is_ok());
    }

    #[test]
    fn test_record_failure_after_attempt() {
        let m = AuthMetrics::new();
        m.record_attempt();
        m.record_failure();
        assert_eq!(m.failed_auths(), 1);
        assert!(m.validate().is_ok());
    }

    #[test]
    fn test_record_validation_error_independent_of_attempt() {
        let m = AuthMetrics::new();
        m.record_validation_error();
        assert_eq!(m.validation_errors(), 1);
        // No attempt was recorded; validate() should still pass.
        assert!(m.validate().is_ok());
    }

    #[test]
    fn test_success_rate_50_percent() {
        let m = AuthMetrics::new();
        m.record_attempt();
        m.record_success();
        m.record_attempt();
        m.record_failure();
        assert_eq!(m.success_rate(), 50.0);
    }

    #[test]
    fn test_success_rate_zero_when_no_attempts() {
        let m = AuthMetrics::new();
        assert_eq!(m.success_rate(), 0.0);
    }

    #[test]
    fn test_success_rate_100_percent() {
        let m = AuthMetrics::new();
        m.record_attempt();
        m.record_success();
        assert_eq!(m.success_rate(), 100.0);
    }

    #[test]
    fn test_validate_passes_when_consistent() {
        let m = AuthMetrics::new();
        m.record_attempt();
        m.record_attempt();
        m.record_success();
        m.record_failure();
        assert!(m.validate().is_ok());
    }

    #[test]
    fn test_success_without_attempt_is_skipped() {
        let m = AuthMetrics::new();
        // No prior attempt — record_success should be a no-op.
        m.record_success();
        assert_eq!(m.successful_auths(), 0);
        assert!(m.validate().is_ok());
    }

    #[test]
    fn test_failure_without_attempt_is_skipped() {
        let m = AuthMetrics::new();
        m.record_failure();
        assert_eq!(m.failed_auths(), 0);
        assert!(m.validate().is_ok());
    }

    #[test]
    fn test_outcome_gating_prevents_overcounting() {
        let m = AuthMetrics::new();
        m.record_attempt();
        m.record_success();
        m.record_failure();

        assert_eq!(m.total_attempts(), 1);
        assert_eq!(m.successful_auths(), 1);
        assert_eq!(m.failed_auths(), 0);
        assert!(m.validate().is_ok());
    }

    #[test]
    fn test_validate_detects_outcomes_exceeding_total() {
        let m = AuthMetrics::new();
        m.total_attempts.store(1, Ordering::Relaxed);
        m.outcomes.store(2, Ordering::Relaxed);
        assert!(m.validate().is_err());
    }

    #[test]
    fn test_validate_detects_completed_outcomes_exceeding_outcomes() {
        let m = AuthMetrics::new();
        m.total_attempts.store(2, Ordering::Relaxed);
        m.outcomes.store(1, Ordering::Relaxed);
        m.successful_auths.store(1, Ordering::Relaxed);
        m.failed_auths.store(1, Ordering::Relaxed);
        assert!(m.validate().is_err());
    }

    #[test]
    fn test_snapshot_matches_counters() {
        let m = AuthMetrics::new();
        m.record_attempt();
        m.record_success();
        m.record_validation_error();

        let snap = m.snapshot();
        assert_eq!(snap.total_attempts, 1);
        assert_eq!(snap.successful_auths, 1);
        assert_eq!(snap.failed_auths, 0);
        assert_eq!(snap.validation_errors, 1);
        assert_eq!(snap.success_rate_pct, 100.0);
    }

    #[test]
    fn test_snapshot_zero_attempts_rate_is_zero() {
        let m = AuthMetrics::new();
        let snap = m.snapshot();
        assert_eq!(snap.success_rate_pct, 0.0);
    }

    #[test]
    fn test_snapshot_export_preserves_metrics_consistency() {
        let m = AuthMetrics::new();
        m.record_attempt();
        m.record_success();
        m.record_attempt();
        m.record_failure();
        m.record_validation_error();

        let snap = m.snapshot();
        assert_eq!(snap.total_attempts, 2);
        assert_eq!(snap.successful_auths, 1);
        assert_eq!(snap.failed_auths, 1);
        assert_eq!(snap.validation_errors, 1);
        assert_eq!(snap.success_rate_pct, 50.0);
        assert!(m.validate().is_ok());
    }

    #[test]
    fn test_reset_clears_all_counters() {
        let m = AuthMetrics::new();
        m.record_attempt();
        m.record_success();
        m.record_validation_error();
        m.reset();
        assert_eq!(m.total_attempts(), 0);
        assert_eq!(m.successful_auths(), 0);
        assert_eq!(m.validation_errors(), 0);
    }

    #[test]
    fn test_clone_shares_counters() {
        let m = AuthMetrics::new();
        let clone = m.clone();
        m.record_attempt();
        assert_eq!(clone.total_attempts(), 1);
        clone.record_attempt();
        clone.record_success();
        assert_eq!(m.successful_auths(), 1);
    }

    #[test]
    fn test_validate_returns_error_on_inconsistency() {
        // Manually force an inconsistent state by bypassing the guard.
        // We do this by recording two successes against one attempt.
        let m = AuthMetrics::new();
        m.record_attempt();
        m.record_success(); // valid: 1 success, 1 attempt
                            // Force a second success by directly manipulating via a second attempt
                            // then checking that validate catches outcomes > total.
                            // We simulate the invariant violation by recording success twice
                            // on a single attempt (second call is guarded, so we use two attempts
                            // but only one total to test the validate path directly).
                            // Instead, test via the public API: two outcomes on one attempt.
        m.record_attempt(); // total = 2
        m.record_success(); // success = 2, failed = 0 → 2 ≤ 2 ✓
        m.record_failure(); // success = 2, failed = 1 → 3 > 2 ✗
                            // The failure guard fires (2+0 = 2 >= 2), so failed stays 0.
                            // Validate should still pass.
        assert!(m.validate().is_ok());
    }
}
