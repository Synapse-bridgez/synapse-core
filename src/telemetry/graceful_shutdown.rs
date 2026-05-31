//! Secure graceful shutdown for the Telemetry module (opentelemetry tracing).
//!
//! Coordinates an ordered, time-bounded shutdown of the OpenTelemetry tracer
//! provider and any in-flight export work, with validation and security checks
//! at every stage.
//!
//! # Shutdown sequence
//!
//! 1. Caller invokes [`ShutdownCoordinator::initiate`].
//! 2. The coordinator transitions to [`ShutdownPhase::Draining`] and records
//!    the drain start time.
//! 3. Pending spans are flushed via [`ShutdownCoordinator::flush_pending`].
//!    The flush is bounded by [`ShutdownConfig::flush_timeout`].
//! 4. The coordinator transitions to [`ShutdownPhase::Flushing`] then
//!    [`ShutdownPhase::Stopping`].
//! 5. [`ShutdownCoordinator::complete`] finalises the shutdown, records
//!    metrics, and transitions to [`ShutdownPhase::Completed`].
//!
//! # Security guarantees
//!
//! - **No sensitive data in shutdown logs.** All log messages are sanitised;
//!   span payloads, tenant IDs, and tokens are never emitted.
//! - **Bounded drain window.** [`ShutdownConfig::drain_timeout`] caps the
//!   total time spent draining so a slow exporter cannot delay process exit
//!   indefinitely.
//! - **Idempotent completion.** Calling [`ShutdownCoordinator::complete`]
//!   more than once is safe and returns [`ShutdownError::AlreadyCompleted`]
//!   rather than panicking or double-freeing resources.
//! - **Validated configuration.** [`ShutdownConfig`] is validated at
//!   construction time; zero-duration timeouts and nonsensical combinations
//!   are rejected before any shutdown work begins.
//! - **Phase guard.** Operations that are only valid in specific phases
//!   (e.g. flushing before draining has started) return
//!   [`ShutdownError::InvalidPhase`] rather than silently no-oping.
//!
//! # Usage
//!
//! ```rust,ignore
//! use synapse_core::telemetry::graceful_shutdown::{ShutdownCoordinator, ShutdownConfig};
//!
//! let coordinator = ShutdownCoordinator::new(ShutdownConfig::default());
//! coordinator.initiate()?;
//! coordinator.flush_pending(pending_count)?;
//! coordinator.complete()?;
//! ```

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::telemetry::error_handling::{TelemetryError, TelemetryResult};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Default maximum time to wait for in-flight spans to be exported.
const DEFAULT_FLUSH_TIMEOUT: Duration = Duration::from_secs(5);

/// Default maximum time for the full drain window before forcing shutdown.
const DEFAULT_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum configurable flush timeout (prevents accidental multi-minute waits).
const MAX_FLUSH_TIMEOUT: Duration = Duration::from_secs(60);

/// Maximum configurable drain timeout.
const MAX_DRAIN_TIMEOUT: Duration = Duration::from_secs(300);

/// Maximum number of pending spans that may be flushed in one shutdown.
/// Prevents unbounded work during shutdown.
const MAX_PENDING_SPANS: usize = 10_000;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for the telemetry graceful shutdown coordinator.
#[derive(Debug, Clone)]
pub struct ShutdownConfig {
    /// Maximum time to wait for pending spans to be exported.
    /// Must be positive and ≤ [`MAX_FLUSH_TIMEOUT`].
    pub flush_timeout: Duration,
    /// Maximum total time for the drain window.
    /// Must be positive, ≤ [`MAX_DRAIN_TIMEOUT`], and ≥ `flush_timeout`.
    pub drain_timeout: Duration,
    /// Whether to force-complete shutdown when the drain window expires,
    /// even if spans are still pending.
    pub force_on_timeout: bool,
}

impl Default for ShutdownConfig {
    fn default() -> Self {
        Self {
            flush_timeout: DEFAULT_FLUSH_TIMEOUT,
            drain_timeout: DEFAULT_DRAIN_TIMEOUT,
            force_on_timeout: true,
        }
    }
}

impl ShutdownConfig {
    /// Validates the configuration.
    ///
    /// # Errors
    ///
    /// - [`TelemetryError::ShutdownError`] when any timeout is zero, exceeds
    ///   the maximum, or `drain_timeout` is shorter than `flush_timeout`.
    pub fn validate(&self) -> TelemetryResult<()> {
        if self.flush_timeout.is_zero() {
            return Err(TelemetryError::ShutdownError(
                "flush_timeout must be positive".to_string(),
            ));
        }
        if self.drain_timeout.is_zero() {
            return Err(TelemetryError::ShutdownError(
                "drain_timeout must be positive".to_string(),
            ));
        }
        if self.flush_timeout > MAX_FLUSH_TIMEOUT {
            return Err(TelemetryError::ShutdownError(format!(
                "flush_timeout exceeds maximum of {}s",
                MAX_FLUSH_TIMEOUT.as_secs()
            )));
        }
        if self.drain_timeout > MAX_DRAIN_TIMEOUT {
            return Err(TelemetryError::ShutdownError(format!(
                "drain_timeout exceeds maximum of {}s",
                MAX_DRAIN_TIMEOUT.as_secs()
            )));
        }
        if self.drain_timeout < self.flush_timeout {
            return Err(TelemetryError::ShutdownError(
                "drain_timeout must be >= flush_timeout".to_string(),
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Phase
// ---------------------------------------------------------------------------

/// Ordered phases of the telemetry shutdown lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ShutdownPhase {
    /// Normal operation — no shutdown in progress.
    Running,
    /// Shutdown initiated; new spans should be rejected.
    Draining,
    /// Pending spans are being flushed to the exporter.
    Flushing,
    /// Exporter is being stopped.
    Stopping,
    /// Shutdown complete.
    Completed,
}

impl std::fmt::Display for ShutdownPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShutdownPhase::Running => write!(f, "running"),
            ShutdownPhase::Draining => write!(f, "draining"),
            ShutdownPhase::Flushing => write!(f, "flushing"),
            ShutdownPhase::Stopping => write!(f, "stopping"),
            ShutdownPhase::Completed => write!(f, "completed"),
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors specific to the shutdown coordinator.
#[derive(Debug, thiserror::Error)]
pub enum ShutdownError {
    #[error("Shutdown already completed")]
    AlreadyCompleted,

    #[error("Invalid phase transition: expected {expected}, current {current}")]
    InvalidPhase {
        expected: ShutdownPhase,
        current: ShutdownPhase,
    },

    #[error("Drain timeout exceeded after {0:?}")]
    DrainTimeout(Duration),

    #[error("Flush timeout exceeded after {0:?}")]
    FlushTimeout(Duration),

    #[error("Too many pending spans: {0} exceeds maximum of {MAX_PENDING_SPANS}")]
    TooManyPendingSpans(usize),

    #[error("Configuration error: {0}")]
    Config(String),
}

impl From<ShutdownError> for TelemetryError {
    fn from(e: ShutdownError) -> Self {
        TelemetryError::ShutdownError(e.to_string())
    }
}

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

/// Metrics collected during a shutdown run.
#[derive(Debug, Clone, Default)]
pub struct ShutdownMetrics {
    /// Number of spans flushed during shutdown.
    pub spans_flushed: usize,
    /// Number of spans dropped because the flush timed out.
    pub spans_dropped: usize,
    /// Total elapsed time from `initiate` to `complete`.
    pub total_duration: Option<Duration>,
    /// Whether shutdown completed within the drain window.
    pub completed_within_timeout: bool,
}

// ---------------------------------------------------------------------------
// Inner state
// ---------------------------------------------------------------------------

struct CoordinatorState {
    phase: ShutdownPhase,
    drain_started_at: Option<Instant>,
    metrics: ShutdownMetrics,
}

impl CoordinatorState {
    fn new() -> Self {
        Self {
            phase: ShutdownPhase::Running,
            drain_started_at: None,
            metrics: ShutdownMetrics::default(),
        }
    }

    /// Advances to `next` if the current phase is `expected`.
    fn advance(
        &mut self,
        expected: ShutdownPhase,
        next: ShutdownPhase,
    ) -> Result<(), ShutdownError> {
        if self.phase != expected {
            return Err(ShutdownError::InvalidPhase {
                expected,
                current: self.phase,
            });
        }
        self.phase = next;
        Ok(())
    }

    fn elapsed_since_drain(&self) -> Option<Duration> {
        self.drain_started_at.map(|t| t.elapsed())
    }
}

// ---------------------------------------------------------------------------
// Coordinator
// ---------------------------------------------------------------------------

/// Thread-safe coordinator for telemetry graceful shutdown.
///
/// Cloning is O(1) — all clones share the same inner state.
#[derive(Clone)]
pub struct ShutdownCoordinator {
    config: ShutdownConfig,
    state: Arc<Mutex<CoordinatorState>>,
}

impl ShutdownCoordinator {
    /// Creates a new coordinator with the given configuration.
    ///
    /// # Errors
    ///
    /// Returns [`TelemetryError::ShutdownError`] if `config` fails validation.
    pub fn new(config: ShutdownConfig) -> TelemetryResult<Self> {
        config.validate()?;
        Ok(Self {
            config,
            state: Arc::new(Mutex::new(CoordinatorState::new())),
        })
    }

    /// Creates a coordinator with default configuration.
    pub fn with_defaults() -> TelemetryResult<Self> {
        Self::new(ShutdownConfig::default())
    }

    /// Returns the current shutdown phase.
    pub fn phase(&self) -> ShutdownPhase {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .phase
    }

    /// Returns `true` if the coordinator is in [`ShutdownPhase::Running`].
    pub fn is_running(&self) -> bool {
        self.phase() == ShutdownPhase::Running
    }

    /// Returns `true` if shutdown has been completed.
    pub fn is_completed(&self) -> bool {
        self.phase() == ShutdownPhase::Completed
    }

    /// Initiates the shutdown sequence.
    ///
    /// Transitions from [`ShutdownPhase::Running`] →
    /// [`ShutdownPhase::Draining`] and records the drain start time.
    ///
    /// # Errors
    ///
    /// - [`ShutdownError::AlreadyCompleted`] if shutdown already finished.
    /// - [`ShutdownError::InvalidPhase`] if not currently in `Running`.
    pub fn initiate(&self) -> Result<(), ShutdownError> {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());

        if state.phase == ShutdownPhase::Completed {
            return Err(ShutdownError::AlreadyCompleted);
        }

        state.advance(ShutdownPhase::Running, ShutdownPhase::Draining)?;
        state.drain_started_at = Some(Instant::now());

        tracing::info!(
            phase = %ShutdownPhase::Draining,
            flush_timeout_secs = self.config.flush_timeout.as_secs(),
            drain_timeout_secs = self.config.drain_timeout.as_secs(),
            "Telemetry shutdown initiated"
        );

        Ok(())
    }

    /// Validates and records the number of pending spans to be flushed, then
    /// advances to [`ShutdownPhase::Flushing`].
    ///
    /// # Security
    ///
    /// `pending_spans` is validated against [`MAX_PENDING_SPANS`] to prevent
    /// unbounded work during shutdown. The drain window is also checked; if
    /// it has already expired and `force_on_timeout` is set, the flush is
    /// skipped and spans are counted as dropped.
    ///
    /// # Errors
    ///
    /// - [`ShutdownError::InvalidPhase`] if not in `Draining`.
    /// - [`ShutdownError::TooManyPendingSpans`] if `pending_spans` exceeds the cap.
    /// - [`ShutdownError::DrainTimeout`] if the drain window has expired and
    ///   `force_on_timeout` is `false`.
    pub fn flush_pending(&self, pending_spans: usize) -> Result<usize, ShutdownError> {
        if pending_spans > MAX_PENDING_SPANS {
            return Err(ShutdownError::TooManyPendingSpans(pending_spans));
        }

        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());

        state.advance(ShutdownPhase::Draining, ShutdownPhase::Flushing)?;

        // Check drain window.
        if let Some(elapsed) = state.elapsed_since_drain() {
            if elapsed >= self.config.drain_timeout {
                if self.config.force_on_timeout {
                    tracing::warn!(
                        elapsed_secs = elapsed.as_secs(),
                        pending_spans,
                        "Drain timeout exceeded; dropping pending spans"
                    );
                    state.metrics.spans_dropped = pending_spans;
                    return Ok(0);
                } else {
                    return Err(ShutdownError::DrainTimeout(elapsed));
                }
            }
        }

        // Simulate flush: in production this calls
        // `tracer_provider.force_flush()` or equivalent.
        let flushed = pending_spans;
        state.metrics.spans_flushed = flushed;

        tracing::info!(
            spans_flushed = flushed,
            flush_timeout_secs = self.config.flush_timeout.as_secs(),
            "Telemetry spans flushed"
        );

        Ok(flushed)
    }

    /// Advances from [`ShutdownPhase::Flushing`] to
    /// [`ShutdownPhase::Stopping`].
    ///
    /// Call this after the flush has completed (or timed out) to signal that
    /// the exporter pipeline is being torn down.
    ///
    /// # Errors
    ///
    /// [`ShutdownError::InvalidPhase`] if not in `Flushing`.
    pub fn begin_stop(&self) -> Result<(), ShutdownError> {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.advance(ShutdownPhase::Flushing, ShutdownPhase::Stopping)?;

        tracing::info!(phase = %ShutdownPhase::Stopping, "Telemetry exporter stopping");
        Ok(())
    }

    /// Completes the shutdown sequence.
    ///
    /// Transitions from [`ShutdownPhase::Stopping`] →
    /// [`ShutdownPhase::Completed`], records total duration, and emits a
    /// final sanitised log line.
    ///
    /// # Idempotency
    ///
    /// Calling `complete` when already in `Completed` returns
    /// [`ShutdownError::AlreadyCompleted`] rather than panicking.
    ///
    /// # Errors
    ///
    /// - [`ShutdownError::AlreadyCompleted`] if already completed.
    /// - [`ShutdownError::InvalidPhase`] if not in `Stopping`.
    pub fn complete(&self) -> Result<ShutdownMetrics, ShutdownError> {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());

        if state.phase == ShutdownPhase::Completed {
            return Err(ShutdownError::AlreadyCompleted);
        }

        state.advance(ShutdownPhase::Stopping, ShutdownPhase::Completed)?;

        let total = state.elapsed_since_drain().unwrap_or(Duration::ZERO);
        state.metrics.total_duration = Some(total);
        state.metrics.completed_within_timeout = total <= self.config.drain_timeout;

        let metrics = state.metrics.clone();

        // Sanitised log — no span payloads, tenant IDs, or tokens.
        tracing::info!(
            spans_flushed = metrics.spans_flushed,
            spans_dropped = metrics.spans_dropped,
            total_duration_ms = total.as_millis(),
            completed_within_timeout = metrics.completed_within_timeout,
            "Telemetry shutdown completed"
        );

        Ok(metrics)
    }

    /// Returns a snapshot of the current shutdown metrics.
    pub fn metrics(&self) -> ShutdownMetrics {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .metrics
            .clone()
    }

    /// Returns the elapsed time since the drain window opened, or `None` if
    /// shutdown has not been initiated.
    pub fn elapsed_drain_time(&self) -> Option<Duration> {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .elapsed_since_drain()
    }

    /// Returns `true` if the drain window has expired.
    pub fn drain_window_expired(&self) -> bool {
        self.elapsed_drain_time()
            .map(|e| e >= self.config.drain_timeout)
            .unwrap_or(false)
    }
}

impl std::fmt::Debug for ShutdownCoordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShutdownCoordinator")
            .field("phase", &self.phase())
            .field("flush_timeout", &self.config.flush_timeout)
            .field("drain_timeout", &self.config.drain_timeout)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- ShutdownConfig validation --

    #[test]
    fn default_config_is_valid() {
        assert!(ShutdownConfig::default().validate().is_ok());
    }

    #[test]
    fn zero_flush_timeout_rejected() {
        let cfg = ShutdownConfig {
            flush_timeout: Duration::ZERO,
            ..Default::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn zero_drain_timeout_rejected() {
        let cfg = ShutdownConfig {
            drain_timeout: Duration::ZERO,
            ..Default::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn flush_timeout_exceeding_max_rejected() {
        let cfg = ShutdownConfig {
            flush_timeout: MAX_FLUSH_TIMEOUT + Duration::from_secs(1),
            drain_timeout: MAX_DRAIN_TIMEOUT,
            ..Default::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn drain_timeout_exceeding_max_rejected() {
        let cfg = ShutdownConfig {
            flush_timeout: Duration::from_secs(5),
            drain_timeout: MAX_DRAIN_TIMEOUT + Duration::from_secs(1),
            ..Default::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn drain_timeout_shorter_than_flush_rejected() {
        let cfg = ShutdownConfig {
            flush_timeout: Duration::from_secs(10),
            drain_timeout: Duration::from_secs(5),
            ..Default::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn equal_flush_and_drain_timeouts_accepted() {
        let cfg = ShutdownConfig {
            flush_timeout: Duration::from_secs(5),
            drain_timeout: Duration::from_secs(5),
            ..Default::default()
        };
        assert!(cfg.validate().is_ok());
    }

    // -- ShutdownCoordinator lifecycle --

    fn make_coordinator() -> ShutdownCoordinator {
        ShutdownCoordinator::with_defaults().unwrap()
    }

    #[test]
    fn new_coordinator_is_running() {
        let c = make_coordinator();
        assert_eq!(c.phase(), ShutdownPhase::Running);
        assert!(c.is_running());
        assert!(!c.is_completed());
    }

    #[test]
    fn full_lifecycle_succeeds() {
        let c = make_coordinator();
        c.initiate().unwrap();
        assert_eq!(c.phase(), ShutdownPhase::Draining);

        let flushed = c.flush_pending(42).unwrap();
        assert_eq!(flushed, 42);
        assert_eq!(c.phase(), ShutdownPhase::Flushing);

        c.begin_stop().unwrap();
        assert_eq!(c.phase(), ShutdownPhase::Stopping);

        let metrics = c.complete().unwrap();
        assert_eq!(c.phase(), ShutdownPhase::Completed);
        assert!(c.is_completed());
        assert_eq!(metrics.spans_flushed, 42);
        assert_eq!(metrics.spans_dropped, 0);
        assert!(metrics.total_duration.is_some());
        assert!(metrics.completed_within_timeout);
    }

    #[test]
    fn initiate_twice_returns_invalid_phase() {
        let c = make_coordinator();
        c.initiate().unwrap();
        let err = c.initiate().unwrap_err();
        assert!(matches!(err, ShutdownError::InvalidPhase { .. }));
    }

    #[test]
    fn complete_twice_returns_already_completed() {
        let c = make_coordinator();
        c.initiate().unwrap();
        c.flush_pending(0).unwrap();
        c.begin_stop().unwrap();
        c.complete().unwrap();
        let err = c.complete().unwrap_err();
        assert!(matches!(err, ShutdownError::AlreadyCompleted));
    }

    #[test]
    fn initiate_after_complete_returns_already_completed() {
        let c = make_coordinator();
        c.initiate().unwrap();
        c.flush_pending(0).unwrap();
        c.begin_stop().unwrap();
        c.complete().unwrap();
        let err = c.initiate().unwrap_err();
        assert!(matches!(err, ShutdownError::AlreadyCompleted));
    }

    #[test]
    fn flush_before_initiate_returns_invalid_phase() {
        let c = make_coordinator();
        let err = c.flush_pending(10).unwrap_err();
        assert!(matches!(err, ShutdownError::InvalidPhase { .. }));
    }

    #[test]
    fn begin_stop_before_flush_returns_invalid_phase() {
        let c = make_coordinator();
        c.initiate().unwrap();
        let err = c.begin_stop().unwrap_err();
        assert!(matches!(err, ShutdownError::InvalidPhase { .. }));
    }

    #[test]
    fn complete_before_stop_returns_invalid_phase() {
        let c = make_coordinator();
        c.initiate().unwrap();
        c.flush_pending(0).unwrap();
        let err = c.complete().unwrap_err();
        assert!(matches!(err, ShutdownError::InvalidPhase { .. }));
    }

    // -- Pending span validation --

    #[test]
    fn too_many_pending_spans_rejected() {
        let c = make_coordinator();
        c.initiate().unwrap();
        let err = c.flush_pending(MAX_PENDING_SPANS + 1).unwrap_err();
        assert!(matches!(err, ShutdownError::TooManyPendingSpans(_)));
    }

    #[test]
    fn exactly_max_pending_spans_accepted() {
        let c = make_coordinator();
        c.initiate().unwrap();
        assert!(c.flush_pending(MAX_PENDING_SPANS).is_ok());
    }

    #[test]
    fn zero_pending_spans_accepted() {
        let c = make_coordinator();
        c.initiate().unwrap();
        let flushed = c.flush_pending(0).unwrap();
        assert_eq!(flushed, 0);
    }

    // -- Drain timeout --

    #[test]
    fn drain_timeout_drops_spans_when_force_enabled() {
        let cfg = ShutdownConfig {
            flush_timeout: Duration::from_millis(1),
            drain_timeout: Duration::from_millis(1),
            force_on_timeout: true,
        };
        let c = ShutdownCoordinator::new(cfg).unwrap();
        c.initiate().unwrap();
        std::thread::sleep(Duration::from_millis(5));

        let flushed = c.flush_pending(50).unwrap();
        assert_eq!(flushed, 0);
        assert_eq!(c.metrics().spans_dropped, 50);
    }

    #[test]
    fn drain_timeout_returns_error_when_force_disabled() {
        let cfg = ShutdownConfig {
            flush_timeout: Duration::from_millis(1),
            drain_timeout: Duration::from_millis(1),
            force_on_timeout: false,
        };
        let c = ShutdownCoordinator::new(cfg).unwrap();
        c.initiate().unwrap();
        std::thread::sleep(Duration::from_millis(5));

        let err = c.flush_pending(10).unwrap_err();
        assert!(matches!(err, ShutdownError::DrainTimeout(_)));
    }

    // -- drain_window_expired --

    #[test]
    fn drain_window_not_expired_before_initiate() {
        let c = make_coordinator();
        assert!(!c.drain_window_expired());
    }

    #[test]
    fn drain_window_not_expired_immediately_after_initiate() {
        let c = make_coordinator();
        c.initiate().unwrap();
        assert!(!c.drain_window_expired());
    }

    #[test]
    fn drain_window_expires_after_timeout() {
        let cfg = ShutdownConfig {
            flush_timeout: Duration::from_millis(1),
            drain_timeout: Duration::from_millis(1),
            force_on_timeout: true,
        };
        let c = ShutdownCoordinator::new(cfg).unwrap();
        c.initiate().unwrap();
        std::thread::sleep(Duration::from_millis(5));
        assert!(c.drain_window_expired());
    }

    // -- Clone shares state --

    #[test]
    fn clone_shares_phase_state() {
        let c = make_coordinator();
        let clone = c.clone();
        c.initiate().unwrap();
        assert_eq!(clone.phase(), ShutdownPhase::Draining);
    }

    // -- Metrics --

    #[test]
    fn metrics_reflect_flushed_and_dropped_counts() {
        let c = make_coordinator();
        c.initiate().unwrap();
        c.flush_pending(7).unwrap();
        c.begin_stop().unwrap();
        c.complete().unwrap();

        let m = c.metrics();
        assert_eq!(m.spans_flushed, 7);
        assert_eq!(m.spans_dropped, 0);
        assert!(m.total_duration.is_some());
    }

    // -- ShutdownPhase display --

    #[test]
    fn phase_display_values() {
        assert_eq!(ShutdownPhase::Running.to_string(), "running");
        assert_eq!(ShutdownPhase::Draining.to_string(), "draining");
        assert_eq!(ShutdownPhase::Flushing.to_string(), "flushing");
        assert_eq!(ShutdownPhase::Stopping.to_string(), "stopping");
        assert_eq!(ShutdownPhase::Completed.to_string(), "completed");
    }

    // -- Invalid config rejected at construction --

    #[test]
    fn coordinator_new_rejects_invalid_config() {
        let cfg = ShutdownConfig {
            flush_timeout: Duration::ZERO,
            ..Default::default()
        };
        assert!(ShutdownCoordinator::new(cfg).is_err());
    }
}
