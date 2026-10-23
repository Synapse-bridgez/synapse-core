//! Error handling for telemetry operations.
//!
//! Provides comprehensive error types and handling strategies for
//! OpenTelemetry tracing operations.

use std::fmt;

/// Comprehensive error type for all telemetry operations.
///
/// This enum consolidates errors from initialization, exporting, validation, pooling,
/// and data export operations. All variants are designed to support graceful degradation
/// without panicking. Use Result<T, TelemetryError> for all fallible telemetry operations.
#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    /// Error initializing the tracer provider.
    ///
    /// This typically indicates a misconfiguration or unavailable OpenTelemetry backend.
    /// The caller should defer telemetry initialization or operate without traces.
    #[error("Failed to initialize tracer: {0}")]
    InitializationError(String),

    /// Error configuring the OTLP exporter.
    ///
    /// Indicates invalid exporter configuration (e.g., invalid endpoint URL, bad TLS setup).
    /// The caller should verify configuration and retry or fall back to a no-op exporter.
    #[error("Failed to configure OTLP exporter: {0}")]
    ExporterConfigError(String),

    /// Error during span export.
    ///
    /// Indicates a temporary or persistent failure to send spans to the telemetry backend.
    /// The caller may retry, buffer spans locally, or degrade gracefully.
    #[error("Failed to export spans: {0}")]
    ExportError(String),

    /// Error during tracer shutdown.
    ///
    /// Indicates a graceful shutdown of the telemetry system could not be completed cleanly.
    /// The caller should log and continue; forced shutdown will clean up resources.
    #[error("Failed to shutdown tracer: {0}")]
    ShutdownError(String),

    /// Invalid endpoint configuration.
    ///
    /// Indicates the endpoint URL failed validation (wrong scheme, malformed URL, too long).
    /// The caller must provide a valid http:// or https:// endpoint.
    #[error("Invalid endpoint: {0}")]
    InvalidEndpoint(String),

    /// Connection error to telemetry backend.
    ///
    /// Indicates a network or I/O failure communicating with the telemetry backend.
    /// The caller should implement retry logic with backoff or switch to degraded mode.
    #[error("Connection error: {0}")]
    ConnectionError(String),

    /// Validation error for telemetry input.
    ///
    /// Indicates span names, attributes, or endpoints failed validation.
    /// The caller must sanitize and validate inputs before retrying.
    #[error("Validation error: {0}")]
    ValidationError(#[from] super::input_validation::ValidationError),

    /// Connection pool is exhausted; all available connections are in use.
    ///
    /// Indicates too many concurrent telemetry operations. The caller should either
    /// reduce concurrency, increase pool size, or defer the operation.
    #[error("Connection pool exhausted: all {0} connections in use")]
    PoolExhausted(usize),

    /// Invalid pool configuration.
    ///
    /// Indicates max_size or other pool parameters are invalid.
    /// The caller should fix the configuration and reinitialize the pool.
    #[error("Invalid pool configuration: {0}")]
    PoolConfigError(String),

    /// Circuit breaker is open; rejecting requests due to too many consecutive failures.
    ///
    /// The telemetry system has detected repeated failures and is protecting against
    /// cascading errors. The caller should wait before retrying, or fall back to
    /// degraded telemetry functionality.
    #[error("Circuit breaker open: too many consecutive failures")]
    CircuitBreakerOpen,

    /// Timeout during telemetry operation.
    ///
    /// Indicates the operation exceeded the configured timeout.
    /// The caller should retry with a longer timeout or abandon the operation.
    #[error("Operation timed out after {0:?}")]
    Timeout(std::time::Duration),

    /// Telemetry payload exceeds maximum allowed size.
    ///
    /// Indicates a batch or record is too large to export. The caller should split
    /// the payload or reduce the number of attributes.
    #[error("Payload exceeds maximum size of {0} bytes")]
    PayloadTooLarge(usize),

    /// Data export buffer overflow; records were discarded.
    ///
    /// Indicates the export buffer reached capacity and older records were dropped.
    /// The caller should increase buffer size or reduce emission rate.
    #[error("Export buffer overflow: oldest records dropped")]
    BufferOverflow,
}

/// Result type for telemetry operations
pub type TelemetryResult<T> = Result<T, TelemetryError>;

/// Handles telemetry errors and determines recovery strategy.
///
/// # Health Check
///
/// Monitors the health of telemetry operations by tracking consecutive errors and
/// determining whether to fail fast or continue with graceful degradation.
/// - **Fail-fast mode**: Stops immediately on any error.
/// - **Threshold mode**: Continues until error count exceeds threshold, then stops.
/// - **Graceful degradation**: Returns `Continue` for transient errors until threshold,
///   allowing the application to proceed with no-op telemetry.
#[derive(Debug, Clone)]
pub struct ErrorHandler {
    /// Whether to fail fast on errors or continue with degraded functionality
    fail_fast: bool,
    /// Maximum number of errors to tolerate before failing
    error_threshold: usize,
    /// Current error count
    error_count: usize,
}

impl ErrorHandler {
    /// Creates a new error handler with default settings (threshold: 10, no fail-fast).
    ///
    /// # Health Check
    ///
    /// Returns a healthy error handler that allows up to 10 consecutive errors
    /// before returning `Stop`. Useful for graceful degradation.
    pub fn new() -> Self {
        Self {
            fail_fast: false,
            error_threshold: 10,
            error_count: 0,
        }
    }

    /// Creates an error handler that fails immediately on any error.
    ///
    /// # Health Check
    ///
    /// Returns a strict error handler that rejects all errors immediately with `Stop`.
    /// Use this for critical telemetry that must not degrade.
    pub fn fail_fast() -> Self {
        Self {
            fail_fast: true,
            error_threshold: 1,
            error_count: 0,
        }
    }

    /// Creates an error handler with custom error threshold.
    ///
    /// # Health Check
    ///
    /// Returns an error handler that tolerates up to `threshold` consecutive errors
    /// before returning `Stop`. The threshold allows for transient failures.
    pub fn with_threshold(threshold: usize) -> Self {
        Self {
            fail_fast: false,
            error_threshold: threshold,
            error_count: 0,
        }
    }

    /// Records an error and determines the recovery action.
    ///
    /// # Health Check
    ///
    /// - `FailFast` on validation or circuit breaker errors (caller should not retry).
    /// - `Stop` when the error threshold is exceeded (graceful degradation).
    /// - `Continue` otherwise, allowing the operation to retry.
    ///
    /// The handler logs each error with counts and threshold for observability.
    pub fn handle_error(&mut self, error: &TelemetryError) -> ErrorAction {
        self.error_count += 1;

        // Log the error
        tracing::warn!(
            error = %error,
            error_count = self.error_count,
            threshold = self.error_threshold,
            "Telemetry error occurred"
        );

        // Determine action based on error type and configuration
        match error {
            TelemetryError::CircuitBreakerOpen => {
                // Circuit breaker errors should always stop attempts
                ErrorAction::Stop
            }
            TelemetryError::ValidationError(_) => {
                // Validation errors indicate bad input, should fail fast
                ErrorAction::FailFast
            }
            _ => {
                if self.fail_fast || self.error_count >= self.error_threshold {
                    ErrorAction::Stop
                } else {
                    ErrorAction::Continue
                }
            }
        }
    }

    /// Resets the error count after successful operation.
    ///
    /// # Health Check
    ///
    /// Call this after a successful telemetry operation to reset the error counter.
    /// This allows the handler to tolerate new transient errors.
    pub fn reset(&mut self) {
        self.error_count = 0;
    }

    /// Returns the current error count.
    ///
    /// # Health Check
    ///
    /// A count of zero indicates healthy operation. Counts approaching the threshold
    /// indicate the exporter may be degrading.
    pub fn error_count(&self) -> usize {
        self.error_count
    }

    /// Checks if the error threshold has been exceeded.
    ///
    /// # Health Check
    ///
    /// Returns true if `error_count >= error_threshold`. When true, the handler will
    /// return `Stop` on the next error, and graceful degradation should be applied.
    pub fn threshold_exceeded(&self) -> bool {
        self.error_count >= self.error_threshold
    }
}

impl Default for ErrorHandler {
    fn default() -> Self {
        Self::new()
    }
}

/// Action to take after handling a telemetry error.
///
/// # Health Check
///
/// Determines how the application should respond to a telemetry error:
/// - `Continue`: The error is transient; retry with backoff (transient failures under threshold).
/// - `Stop`: Gracefully degrade to no-op telemetry; do not retry (threshold exceeded or circuit open).
/// - `FailFast`: Fatal error; do not retry (validation or configuration error).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorAction {
    /// Continue operation despite error
    Continue,
    /// Stop operation gracefully
    Stop,
    /// Fail immediately
    FailFast,
}

/// Converts OpenTelemetry errors to TelemetryError
impl From<opentelemetry::trace::TraceError> for TelemetryError {
    fn from(error: opentelemetry::trace::TraceError) -> Self {
        TelemetryError::ExportError(error.to_string())
    }
}

/// Formats telemetry errors for user-friendly display
impl fmt::Display for ErrorAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorAction::Continue => write!(f, "continue"),
            ErrorAction::Stop => write!(f, "stop"),
            ErrorAction::FailFast => write!(f, "fail_fast"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_handler_default() {
        let handler = ErrorHandler::new();
        assert_eq!(handler.error_count(), 0);
        assert!(!handler.threshold_exceeded());
    }

    #[test]
    fn test_error_handler_fail_fast() {
        let mut handler = ErrorHandler::fail_fast();
        let error = TelemetryError::ExportError("test".to_string());
        let action = handler.handle_error(&error);
        assert_eq!(action, ErrorAction::Stop);
    }

    #[test]
    fn test_error_handler_threshold() {
        let mut handler = ErrorHandler::with_threshold(3);
        let error = TelemetryError::ExportError("test".to_string());

        // First two errors should continue
        assert_eq!(handler.handle_error(&error), ErrorAction::Continue);
        assert_eq!(handler.handle_error(&error), ErrorAction::Continue);

        // Third error should stop
        assert_eq!(handler.handle_error(&error), ErrorAction::Stop);
        assert!(handler.threshold_exceeded());
    }

    #[test]
    fn test_error_handler_reset() {
        let mut handler = ErrorHandler::with_threshold(5);
        let error = TelemetryError::ExportError("test".to_string());

        handler.handle_error(&error);
        handler.handle_error(&error);
        assert_eq!(handler.error_count(), 2);

        handler.reset();
        assert_eq!(handler.error_count(), 0);
        assert!(!handler.threshold_exceeded());
    }

    #[test]
    fn test_circuit_breaker_error_always_stops() {
        let mut handler = ErrorHandler::new();
        let error = TelemetryError::CircuitBreakerOpen;
        let action = handler.handle_error(&error);
        assert_eq!(action, ErrorAction::Stop);
    }

    #[test]
    fn test_validation_error_fails_fast() {
        let mut handler = ErrorHandler::new();
        let validation_error =
            super::super::input_validation::ValidationError::EmptyValue("test".to_string());
        let error = TelemetryError::ValidationError(validation_error);
        let action = handler.handle_error(&error);
        assert_eq!(action, ErrorAction::FailFast);
    }

    #[test]
    fn test_error_types() {
        let init_err = TelemetryError::InitializationError("init failed".to_string());
        assert!(init_err.to_string().contains("initialize tracer"));

        let export_err = TelemetryError::ExporterConfigError("config failed".to_string());
        assert!(export_err.to_string().contains("configure OTLP"));

        let conn_err = TelemetryError::ConnectionError("connection failed".to_string());
        assert!(conn_err.to_string().contains("Connection error"));
    }

    #[test]
    fn test_timeout_error() {
        let timeout = std::time::Duration::from_secs(30);
        let error = TelemetryError::Timeout(timeout);
        assert!(error.to_string().contains("timed out"));
    }
}
