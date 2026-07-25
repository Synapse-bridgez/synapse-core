//! Rate limiting for telemetry module.
//!
//! Provides per-event-type token-bucket rate limiting for telemetry operations
//! (traces, metrics, and events). Uses the lock-free [`RateLimiter`] from
//! [`crate::cache::rate_limiting`] so there is no duplicated token-bucket logic.
//!
//! Rate limiting is non-fatal: when a rate limit is exceeded, the event is
//! dropped and a warning is logged. This prevents telemetry from overwhelming
//! the export pipeline while ensuring the application continues to function.
//!
//! # Non-Fatal Behavior
//!
//! - All rate-limit overflows produce a warning log, never a panic.
//! - The metrics collector records acquired/rejected counts so operators can
//!   observe throttling in dashboards.
//!
//! # Limits
//!
//! | Event type | Default limit | Window |
//! |------------|--------------|--------|
//! | Trace      | 1000         | 60 s   |
//! | Metric     | 5000         | 60 s   |
//! | Event      | 500          | 60 s   |
//!
//! All limits are configurable via [`TelemetryRateLimitConfig`].

use std::time::Duration;

use crate::cache::rate_limiting::{RateLimitConfig, RateLimitStrategy, RateLimiter};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default maximum traces per window.
const DEFAULT_TRACE_LIMIT: u32 = 1000;

/// Default maximum metrics per window.
const DEFAULT_METRIC_LIMIT: u32 = 5000;

/// Default maximum events per window.
const DEFAULT_EVENT_LIMIT: u32 = 500;

/// Default rate-limit window for all telemetry event types.
const DEFAULT_WINDOW: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for telemetry-layer rate limiting.
///
/// Each event type (trace, metric, event) has an independent token bucket so
/// a burst of one type does not starve the others.
#[derive(Debug, Clone)]
pub struct TelemetryRateLimitConfig {
    /// Maximum traces allowed per window.
    pub trace_limit: u32,
    /// Maximum metrics allowed per window.
    pub metric_limit: u32,
    /// Maximum events allowed per window.
    pub event_limit: u32,
    /// Duration of the rate-limit window for all event types.
    pub window: Duration,
}

impl Default for TelemetryRateLimitConfig {
    fn default() -> Self {
        Self {
            trace_limit: DEFAULT_TRACE_LIMIT,
            metric_limit: DEFAULT_METRIC_LIMIT,
            event_limit: DEFAULT_EVENT_LIMIT,
            window: DEFAULT_WINDOW,
        }
    }
}

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

/// Snapshot of telemetry rate-limiting metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TelemetryRateLimitMetrics {
    /// Number of traces acquired (allowed through).
    pub traces_acquired: u64,
    /// Number of traces rejected (rate-limited).
    pub traces_rejected: u64,
    /// Number of metrics acquired (allowed through).
    pub metrics_acquired: u64,
    /// Number of metrics rejected (rate-limited).
    pub metrics_rejected: u64,
    /// Number of events acquired (allowed through).
    pub events_acquired: u64,
    /// Number of events rejected (rate-limited).
    pub events_rejected: u64,
}

// ---------------------------------------------------------------------------
// TelemetryRateLimiter
// ---------------------------------------------------------------------------

/// Per-event-type rate limiter for telemetry operations.
///
/// Maintains three independent token buckets for traces, metrics, and events.
/// Cloning is O(1) — all clones share the same buckets via [`Arc`].
///
/// # Non-Fatal Behavior
///
/// When a bucket is exhausted, the event is dropped, a warning is logged, and
/// the rejection is recorded in metrics. The application never panics.
#[derive(Clone)]
pub struct TelemetryRateLimiter {
    config: TelemetryRateLimitConfig,
    trace: RateLimiter,
    metric: RateLimiter,
    event: RateLimiter,
}

impl std::fmt::Debug for TelemetryRateLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelemetryRateLimiter")
            .field("trace_limit", &self.config.trace_limit)
            .field("metric_limit", &self.config.metric_limit)
            .field("event_limit", &self.config.event_limit)
            .field("window", &self.config.window)
            .finish()
    }
}

impl TelemetryRateLimiter {
    /// Creates a new telemetry rate limiter with default configuration.
    ///
    /// Equivalent to `Self::with_config(TelemetryRateLimitConfig::default())`.
    pub fn new() -> Self {
        Self::with_config(TelemetryRateLimitConfig::default())
    }

    /// Creates a new telemetry rate limiter with a custom configuration.
    ///
    /// Each event type gets its own token bucket configured with the
    /// corresponding limit from `config`.
    pub fn with_config(config: TelemetryRateLimitConfig) -> Self {
        let trace = RateLimiter::with_config(RateLimitConfig {
            max_requests: config.trace_limit,
            window: config.window,
            strategy: RateLimitStrategy::TokenBucket,
        });
        let metric = RateLimiter::with_config(RateLimitConfig {
            max_requests: config.metric_limit,
            window: config.window,
            strategy: RateLimitStrategy::TokenBucket,
        });
        let event = RateLimiter::with_config(RateLimitConfig {
            max_requests: config.event_limit,
            window: config.window,
            strategy: RateLimitStrategy::TokenBucket,
        });

        Self {
            config,
            trace,
            metric,
            event,
        }
    }

    /// Attempts to acquire a token for a trace event.
    ///
    /// # Returns
    /// `true` if the trace is within the rate limit and should be processed;
    /// `false` if the trace has been rate-limited (dropped).
    ///
    /// # Non-Fatal Behavior
    /// When the trace bucket is exhausted, a warning is logged and `false` is
    /// returned. The caller should drop the trace gracefully.
    pub fn try_acquire_trace(&self) -> bool {
        if self.trace.try_acquire() {
            true
        } else {
            tracing::warn!(
                trace_limit = self.config.trace_limit,
                window_ms = self.config.window.as_millis(),
                "Telemetry trace rate limit exceeded — dropping trace"
            );
            false
        }
    }

    /// Attempts to acquire a token for a metric event.
    ///
    /// # Returns
    /// `true` if the metric is within the rate limit and should be processed;
    /// `false` if the metric has been rate-limited (dropped).
    ///
    /// # Non-Fatal Behavior
    /// When the metric bucket is exhausted, a warning is logged and `false` is
    /// returned. The caller should drop the metric gracefully.
    pub fn try_acquire_metric(&self) -> bool {
        if self.metric.try_acquire() {
            true
        } else {
            tracing::warn!(
                metric_limit = self.config.metric_limit,
                window_ms = self.config.window.as_millis(),
                "Telemetry metric rate limit exceeded — dropping metric"
            );
            false
        }
    }

    /// Attempts to acquire a token for an event (log/event).
    ///
    /// # Returns
    /// `true` if the event is within the rate limit and should be processed;
    /// `false` if the event has been rate-limited (dropped).
    ///
    /// # Non-Fatal Behavior
    /// When the event bucket is exhausted, a warning is logged and `false` is
    /// returned. The caller should drop the event gracefully.
    pub fn try_acquire_event(&self) -> bool {
        if self.event.try_acquire() {
            true
        } else {
            tracing::warn!(
                event_limit = self.config.event_limit,
                window_ms = self.config.window.as_millis(),
                "Telemetry event rate limit exceeded — dropping event"
            );
            false
        }
    }

    /// Attempts to acquire a token for any telemetry event, inferring the
    /// bucket from the event type.
    ///
    /// # Returns
    /// `true` if the event is within the rate limit; `false` otherwise.
    ///
    /// # Non-Fatal Behavior
    /// See per-type methods for non-fatal guarantees.
    pub fn try_acquire(&self, record_type: &crate::telemetry::data_export::RecordType) -> bool {
        match record_type {
            crate::telemetry::data_export::RecordType::Trace => self.try_acquire_trace(),
            crate::telemetry::data_export::RecordType::Metric => self.try_acquire_metric(),
            crate::telemetry::data_export::RecordType::Event => self.try_acquire_event(),
        }
    }

    /// Returns the number of remaining trace tokens.
    pub fn remaining_traces(&self) -> u32 {
        self.trace.available_tokens()
    }

    /// Returns the number of remaining metric tokens.
    pub fn remaining_metrics(&self) -> u32 {
        self.metric.available_tokens()
    }

    /// Returns the number of remaining event tokens.
    pub fn remaining_events(&self) -> u32 {
        self.event.available_tokens()
    }

    /// Returns a metrics snapshot of all buckets.
    ///
    /// This combines the low-level metrics from each underlying token bucket
    /// into a single [`TelemetryRateLimitMetrics`] struct.
    pub fn metrics(&self) -> TelemetryRateLimitMetrics {
        let trace_m = self.trace.metrics();
        let metric_m = self.metric.metrics();
        let event_m = self.event.metrics();

        TelemetryRateLimitMetrics {
            traces_acquired: trace_m.acquired_requests(),
            traces_rejected: trace_m.rejected_requests(),
            metrics_acquired: metric_m.acquired_requests(),
            metrics_rejected: metric_m.rejected_requests(),
            events_acquired: event_m.acquired_requests(),
            events_rejected: event_m.rejected_requests(),
        }
    }

    /// Resets all rate-limit buckets to full capacity.
    ///
    /// Intended for testing or manual operator intervention.
    pub fn reset_all(&self) {
        self.trace.reset();
        self.metric.reset();
        self.event.reset();
    }

    /// Checks whether any of the telemetry buckets are currently exhausted.
    ///
    /// # Returns
    /// `true` if at least one bucket is exhausted (rate limit is being hit).
    /// This can be used for health-check or backpressure signaling.
    pub fn any_exhausted(&self) -> bool {
        self.remaining_traces() == 0
            || self.remaining_metrics() == 0
            || self.remaining_events() == 0
    }
}

impl Default for TelemetryRateLimiter {
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

    // -----------------------------------------------------------------------
    // Configuration
    // -----------------------------------------------------------------------

    #[test]
    fn test_default_config_values() {
        let config = TelemetryRateLimitConfig::default();
        assert_eq!(config.trace_limit, DEFAULT_TRACE_LIMIT);
        assert_eq!(config.metric_limit, DEFAULT_METRIC_LIMIT);
        assert_eq!(config.event_limit, DEFAULT_EVENT_LIMIT);
        assert_eq!(config.window, DEFAULT_WINDOW);
    }

    #[test]
    fn test_custom_config_values() {
        let config = TelemetryRateLimitConfig {
            trace_limit: 100,
            metric_limit: 200,
            event_limit: 50,
            window: Duration::from_secs(30),
        };
        assert_eq!(config.trace_limit, 100);
        assert_eq!(config.metric_limit, 200);
        assert_eq!(config.event_limit, 50);
        assert_eq!(config.window.as_secs(), 30);
    }

    // -----------------------------------------------------------------------
    // Trace rate limiting
    // -----------------------------------------------------------------------

    #[test]
    fn test_trace_within_limit_succeeds() {
        let limiter = TelemetryRateLimiter::with_config(TelemetryRateLimitConfig {
            trace_limit: 5,
            ..Default::default()
        });

        for i in 0..5 {
            assert!(
                limiter.try_acquire_trace(),
                "Trace {} should be acquired",
                i + 1
            );
        }
    }

    #[test]
    fn test_trace_exceeding_limit_fails() {
        let limiter = TelemetryRateLimiter::with_config(TelemetryRateLimitConfig {
            trace_limit: 3,
            ..Default::default()
        });

        assert!(limiter.try_acquire_trace());
        assert!(limiter.try_acquire_trace());
        assert!(limiter.try_acquire_trace());
        assert!(!limiter.try_acquire_trace(), "4th trace should be rejected");
    }

    #[test]
    fn test_trace_limit_resets_after_window() {
        let limiter = TelemetryRateLimiter::with_config(TelemetryRateLimitConfig {
            trace_limit: 2,
            window: Duration::from_millis(50),
            ..Default::default()
        });

        assert!(limiter.try_acquire_trace());
        assert!(limiter.try_acquire_trace());
        assert!(!limiter.try_acquire_trace());

        std::thread::sleep(Duration::from_millis(60));

        assert!(limiter.try_acquire_trace(), "Should reset after window");
    }

    #[test]
    fn test_trace_burst_at_limit_boundary() {
        let limiter = TelemetryRateLimiter::with_config(TelemetryRateLimitConfig {
            trace_limit: 10,
            ..Default::default()
        });

        for i in 0..10 {
            assert!(
                limiter.try_acquire_trace(),
                "Trace {} should succeed",
                i + 1
            );
        }
        assert!(!limiter.try_acquire_trace(), "11th trace should fail");
    }

    // -----------------------------------------------------------------------
    // Metric rate limiting
    // -----------------------------------------------------------------------

    #[test]
    fn test_metric_within_limit_succeeds() {
        let limiter = TelemetryRateLimiter::with_config(TelemetryRateLimitConfig {
            metric_limit: 5,
            ..Default::default()
        });

        for i in 0..5 {
            assert!(
                limiter.try_acquire_metric(),
                "Metric {} should be acquired",
                i + 1
            );
        }
    }

    #[test]
    fn test_metric_exceeding_limit_fails() {
        let limiter = TelemetryRateLimiter::with_config(TelemetryRateLimitConfig {
            metric_limit: 2,
            ..Default::default()
        });

        assert!(limiter.try_acquire_metric());
        assert!(limiter.try_acquire_metric());
        assert!(
            !limiter.try_acquire_metric(),
            "3rd metric should be rejected"
        );
    }

    // -----------------------------------------------------------------------
    // Event rate limiting
    // -----------------------------------------------------------------------

    #[test]
    fn test_event_within_limit_succeeds() {
        let limiter = TelemetryRateLimiter::with_config(TelemetryRateLimitConfig {
            event_limit: 3,
            ..Default::default()
        });

        assert!(limiter.try_acquire_event());
        assert!(limiter.try_acquire_event());
        assert!(limiter.try_acquire_event());
    }

    #[test]
    fn test_event_exceeding_limit_fails() {
        let limiter = TelemetryRateLimiter::with_config(TelemetryRateLimitConfig {
            event_limit: 1,
            ..Default::default()
        });

        assert!(limiter.try_acquire_event());
        assert!(!limiter.try_acquire_event(), "2nd event should be rejected");
    }

    // -----------------------------------------------------------------------
    // Independent buckets (one type cannot starve another)
    // -----------------------------------------------------------------------

    #[test]
    fn test_types_have_independent_buckets() {
        let limiter = TelemetryRateLimiter::with_config(TelemetryRateLimitConfig {
            trace_limit: 1,
            metric_limit: 1,
            event_limit: 1,
            ..Default::default()
        });

        // Exhaust trace bucket
        assert!(limiter.try_acquire_trace());
        assert!(!limiter.try_acquire_trace());

        // Metric should still be available
        assert!(limiter.try_acquire_metric(), "Metric bucket is independent");
        assert!(!limiter.try_acquire_metric());

        // Event should still be available
        assert!(limiter.try_acquire_event(), "Event bucket is independent");
        assert!(!limiter.try_acquire_event());

        // Traces remain exhausted
        assert!(!limiter.try_acquire_trace());
    }

    #[test]
    fn test_one_type_exhausted_does_not_affect_others() {
        let limiter = TelemetryRateLimiter::with_config(TelemetryRateLimitConfig {
            trace_limit: 2,
            metric_limit: 100,
            event_limit: 100,
            ..Default::default()
        });

        // Exhaust traces only
        assert!(limiter.try_acquire_trace());
        assert!(limiter.try_acquire_trace());
        assert!(!limiter.try_acquire_trace());

        // Metrics and events should still work fine
        assert!(limiter.try_acquire_metric());
        assert!(limiter.try_acquire_event());
    }

    // -----------------------------------------------------------------------
    // try_acquire with RecordType
    // -----------------------------------------------------------------------

    #[test]
    fn test_try_acquire_with_record_type_trace() {
        let limiter = TelemetryRateLimiter::with_config(TelemetryRateLimitConfig {
            trace_limit: 1,
            ..Default::default()
        });

        assert!(limiter.try_acquire(&crate::telemetry::data_export::RecordType::Trace));
        assert!(!limiter.try_acquire(&crate::telemetry::data_export::RecordType::Trace));
    }

    #[test]
    fn test_try_acquire_with_record_type_metric() {
        let limiter = TelemetryRateLimiter::with_config(TelemetryRateLimitConfig {
            metric_limit: 1,
            ..Default::default()
        });

        assert!(limiter.try_acquire(&crate::telemetry::data_export::RecordType::Metric));
        assert!(!limiter.try_acquire(&crate::telemetry::data_export::RecordType::Metric));
    }

    #[test]
    fn test_try_acquire_with_record_type_event() {
        let limiter = TelemetryRateLimiter::with_config(TelemetryRateLimitConfig {
            event_limit: 1,
            ..Default::default()
        });

        assert!(limiter.try_acquire(&crate::telemetry::data_export::RecordType::Event));
        assert!(!limiter.try_acquire(&crate::telemetry::data_export::RecordType::Event));
    }

    // -----------------------------------------------------------------------
    // Remaining tokens
    // -----------------------------------------------------------------------

    #[test]
    fn test_remaining_traces_decrements_on_acquire() {
        let limiter = TelemetryRateLimiter::with_config(TelemetryRateLimitConfig {
            trace_limit: 10,
            ..Default::default()
        });

        assert_eq!(limiter.remaining_traces(), 10);
        limiter.try_acquire_trace();
        assert_eq!(limiter.remaining_traces(), 9);
        limiter.try_acquire_trace();
        assert_eq!(limiter.remaining_traces(), 8);
    }

    #[test]
    fn test_remaining_metrics_decrements_on_acquire() {
        let limiter = TelemetryRateLimiter::with_config(TelemetryRateLimitConfig {
            metric_limit: 5,
            ..Default::default()
        });

        assert_eq!(limiter.remaining_metrics(), 5);
        limiter.try_acquire_metric();
        assert_eq!(limiter.remaining_metrics(), 4);
    }

    #[test]
    fn test_remaining_events_decrements_on_acquire() {
        let limiter = TelemetryRateLimiter::with_config(TelemetryRateLimitConfig {
            event_limit: 3,
            ..Default::default()
        });

        assert_eq!(limiter.remaining_events(), 3);
        limiter.try_acquire_event();
        assert_eq!(limiter.remaining_events(), 2);
    }

    #[test]
    fn test_remaining_zero_when_exhausted() {
        let limiter = TelemetryRateLimiter::with_config(TelemetryRateLimitConfig {
            trace_limit: 1,
            metric_limit: 2,
            event_limit: 3,
            ..Default::default()
        });

        limiter.try_acquire_trace();
        assert_eq!(limiter.remaining_traces(), 0);

        // Metrics and events still have tokens
        assert!(limiter.remaining_metrics() > 0);
        assert!(limiter.remaining_events() > 0);
    }

    // -----------------------------------------------------------------------
    // Metrics
    // -----------------------------------------------------------------------

    #[test]
    fn test_metrics_trace_acquire_and_reject() {
        let limiter = TelemetryRateLimiter::with_config(TelemetryRateLimitConfig {
            trace_limit: 2,
            ..Default::default()
        });

        assert!(limiter.try_acquire_trace());
        assert!(limiter.try_acquire_trace());
        assert!(!limiter.try_acquire_trace());

        let m = limiter.metrics();
        assert_eq!(m.traces_acquired, 2);
        assert_eq!(m.traces_rejected, 1);
    }

    #[test]
    fn test_metrics_metric_acquire_and_reject() {
        let limiter = TelemetryRateLimiter::with_config(TelemetryRateLimitConfig {
            metric_limit: 1,
            ..Default::default()
        });

        assert!(limiter.try_acquire_metric());
        assert!(!limiter.try_acquire_metric());

        let m = limiter.metrics();
        assert_eq!(m.metrics_acquired, 1);
        assert_eq!(m.metrics_rejected, 1);
    }

    #[test]
    fn test_metrics_event_acquire_and_reject() {
        let limiter = TelemetryRateLimiter::with_config(TelemetryRateLimitConfig {
            event_limit: 3,
            ..Default::default()
        });

        assert!(limiter.try_acquire_event());
        assert!(limiter.try_acquire_event());
        assert!(limiter.try_acquire_event());
        assert!(!limiter.try_acquire_event());

        let m = limiter.metrics();
        assert_eq!(m.events_acquired, 3);
        assert_eq!(m.events_rejected, 1);
    }

    #[test]
    fn test_metrics_all_types_combined() {
        let limiter = TelemetryRateLimiter::with_config(TelemetryRateLimitConfig {
            trace_limit: 3,
            metric_limit: 2,
            event_limit: 1,
            ..Default::default()
        });

        // Traces: 3 acquired, 0 rejected
        assert!(limiter.try_acquire_trace());
        assert!(limiter.try_acquire_trace());
        assert!(limiter.try_acquire_trace());

        // Metrics: 2 acquired, 1 rejected
        assert!(limiter.try_acquire_metric());
        assert!(limiter.try_acquire_metric());
        assert!(!limiter.try_acquire_metric());

        // Events: 1 acquired, 2 rejected
        assert!(limiter.try_acquire_event());
        assert!(!limiter.try_acquire_event());
        assert!(!limiter.try_acquire_event());

        let m = limiter.metrics();
        assert_eq!(m.traces_acquired, 3);
        assert_eq!(m.traces_rejected, 0);
        assert_eq!(m.metrics_acquired, 2);
        assert_eq!(m.metrics_rejected, 1);
        assert_eq!(m.events_acquired, 1);
        assert_eq!(m.events_rejected, 2);
    }

    #[test]
    fn test_metrics_default_all_zero() {
        let limiter = TelemetryRateLimiter::new();
        let m = limiter.metrics();
        assert_eq!(m.traces_acquired, 0);
        assert_eq!(m.traces_rejected, 0);
        assert_eq!(m.metrics_acquired, 0);
        assert_eq!(m.metrics_rejected, 0);
        assert_eq!(m.events_acquired, 0);
        assert_eq!(m.events_rejected, 0);
    }

    // -----------------------------------------------------------------------
    // Reset
    // -----------------------------------------------------------------------

    #[test]
    fn test_reset_all_restores_all_buckets() {
        let limiter = TelemetryRateLimiter::with_config(TelemetryRateLimitConfig {
            trace_limit: 5,
            metric_limit: 10,
            event_limit: 3,
            ..Default::default()
        });

        // Exhaust all buckets
        for _ in 0..5 {
            limiter.try_acquire_trace();
        }
        for _ in 0..10 {
            limiter.try_acquire_metric();
        }
        for _ in 0..3 {
            limiter.try_acquire_event();
        }

        assert_eq!(limiter.remaining_traces(), 0);
        assert_eq!(limiter.remaining_metrics(), 0);
        assert_eq!(limiter.remaining_events(), 0);

        limiter.reset_all();

        assert_eq!(limiter.remaining_traces(), 5);
        assert_eq!(limiter.remaining_metrics(), 10);
        assert_eq!(limiter.remaining_events(), 3);
    }

    // -----------------------------------------------------------------------
    // any_exhausted
    // -----------------------------------------------------------------------

    #[test]
    fn test_any_exhausted_false_initially() {
        let limiter = TelemetryRateLimiter::new();
        assert!(!limiter.any_exhausted());
    }

    #[test]
    fn test_any_exhausted_true_when_traces_exhausted() {
        let limiter = TelemetryRateLimiter::with_config(TelemetryRateLimitConfig {
            trace_limit: 1,
            ..Default::default()
        });
        limiter.try_acquire_trace();
        assert!(limiter.any_exhausted());
    }

    #[test]
    fn test_any_exhausted_true_when_metrics_exhausted() {
        let limiter = TelemetryRateLimiter::with_config(TelemetryRateLimitConfig {
            metric_limit: 1,
            ..Default::default()
        });
        limiter.try_acquire_metric();
        assert!(limiter.any_exhausted());
    }

    #[test]
    fn test_any_exhausted_true_when_events_exhausted() {
        let limiter = TelemetryRateLimiter::with_config(TelemetryRateLimitConfig {
            event_limit: 1,
            ..Default::default()
        });
        limiter.try_acquire_event();
        assert!(limiter.any_exhausted());
    }

    #[test]
    fn test_any_exhausted_false_when_all_have_tokens() {
        let limiter = TelemetryRateLimiter::with_config(TelemetryRateLimitConfig {
            trace_limit: 5,
            metric_limit: 5,
            event_limit: 5,
            ..Default::default()
        });
        limiter.try_acquire_trace(); // 4 remain
        assert!(!limiter.any_exhausted());
    }

    // -----------------------------------------------------------------------
    // Clone shares state
    // -----------------------------------------------------------------------

    #[test]
    fn test_clone_shares_trace_bucket() {
        let limiter = TelemetryRateLimiter::with_config(TelemetryRateLimitConfig {
            trace_limit: 4,
            ..Default::default()
        });
        let clone = limiter.clone();

        limiter.try_acquire_trace();
        limiter.try_acquire_trace();
        assert_eq!(clone.remaining_traces(), 2);
    }

    #[test]
    fn test_clone_shares_metric_bucket() {
        let limiter = TelemetryRateLimiter::with_config(TelemetryRateLimitConfig {
            metric_limit: 3,
            ..Default::default()
        });
        let clone = limiter.clone();

        limiter.try_acquire_metric();
        assert_eq!(clone.remaining_metrics(), 2);
    }

    #[test]
    fn test_clone_shares_event_bucket() {
        let limiter = TelemetryRateLimiter::with_config(TelemetryRateLimitConfig {
            event_limit: 2,
            ..Default::default()
        });
        let clone = limiter.clone();

        limiter.try_acquire_event();
        limiter.try_acquire_event();
        assert_eq!(clone.remaining_events(), 0);
    }

    // -----------------------------------------------------------------------
    // Edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_zero_limits_reject_everything() {
        let limiter = TelemetryRateLimiter::with_config(TelemetryRateLimitConfig {
            trace_limit: 0,
            metric_limit: 0,
            event_limit: 0,
            ..Default::default()
        });

        assert!(!limiter.try_acquire_trace());
        assert!(!limiter.try_acquire_metric());
        assert!(!limiter.try_acquire_event());
    }

    #[test]
    fn test_default_trait_equals_new() {
        let a = TelemetryRateLimiter::new();
        let b = TelemetryRateLimiter::default();
        assert_eq!(a.remaining_traces(), b.remaining_traces());
        assert_eq!(a.remaining_metrics(), b.remaining_metrics());
        assert_eq!(a.remaining_events(), b.remaining_events());
    }

    #[test]
    fn test_config_default_trait() {
        let a = TelemetryRateLimitConfig::default();
        let b = TelemetryRateLimitConfig::default();
        assert_eq!(a.trace_limit, b.trace_limit);
        assert_eq!(a.metric_limit, b.metric_limit);
        assert_eq!(a.event_limit, b.event_limit);
        assert_eq!(a.window, b.window);
    }

    #[test]
    fn test_large_window_does_not_overflow() {
        let limiter = TelemetryRateLimiter::with_config(TelemetryRateLimitConfig {
            trace_limit: u32::MAX,
            window: Duration::from_secs(3600),
            ..Default::default()
        });

        // Acquire a reasonable number — should not panic or overflow
        for _ in 0..1000 {
            assert!(limiter.try_acquire_trace());
        }
    }
}
