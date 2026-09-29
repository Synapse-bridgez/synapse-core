//! Error handling for cache operations.
//!
//! Provides structured error types for Redis cache operations with support for
//! distinguishing between cache misses (not an error) and actual errors.
//!
//! # Graceful degradation
//!
//! When Redis is unavailable, cache-dependent code paths must not hard-fail the
//! request. Instead they should fall back to a slower but correct path (e.g.
//! direct DB reads) or fail safely with stricter defaults (e.g. rate limiting
//! fails closed-ish). To make the blast radius of a Redis outage observable, all
//! Redis-dependent components emit the same "degraded mode" signal via
//! [`CacheError::is_degradable`] and [`emit_degraded_mode`].

/// Errors that can occur during cache operations
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    /// Redis connection or network failure
    #[error("Cache connection failed: {0}")]
    ConnectionFailed(String),

    /// Key not found in cache (not an error, just a cache miss)
    #[error("Key not found in cache")]
    KeyNotFound,

    /// Serialization or deserialization error
    #[error("Serialization error: {0}")]
    SerializationError(String),

    /// Operation timeout
    #[error("Cache operation timed out")]
    Timeout,

    /// Circuit breaker is open
    #[error("Cache circuit breaker is open")]
    CircuitBreakerOpen,

    /// Validation error (input validation failed)
    #[error("Cache validation error: {0}")]
    ValidationError(String),
}

impl CacheError {
    /// Returns `true` when this error indicates Redis is unavailable and the
    /// caller should switch to its documented degraded-mode fallback rather
    /// than hard-failing the request.
    ///
    /// A cache miss ([`CacheError::KeyNotFound`]) is never a degradation, and
    /// input validation errors are caller bugs, not infrastructure outages.
    pub fn is_degradable(&self) -> bool {
        matches!(
            self,
            CacheError::ConnectionFailed(_)
                | CacheError::Timeout
                | CacheError::CircuitBreakerOpen
        )
    }

    /// Alias for [`CacheError::is_degradable`] using the unavailability
    /// terminology, so callers can express intent either way.
    pub fn is_unavailability(&self) -> bool {
        self.is_degradable()
    }

    /// Stable, low-cardinality label used for the shared degraded-mode metric
    /// and log line so operators can see the full blast radius of a Redis
    /// outage across every component.
    pub fn degraded_reason(&self) -> &'static str {
        match self {
            CacheError::ConnectionFailed(_) => "connection_failed",
            CacheError::Timeout => "timeout",
            CacheError::CircuitBreakerOpen => "circuit_breaker_open",
            CacheError::KeyNotFound => "cache_miss",
            CacheError::SerializationError(_) => "serialization_error",
            CacheError::ValidationError(_) => "validation_error",
        }
    }
}

/// True when a raw Redis error means Redis is unavailable (see
/// [`CacheError::is_unavailability`]). Validation / (de)serialization
/// errors raised on our side are not unavailability.
pub fn is_redis_unavailable(error: &redis::RedisError) -> bool {
    if matches!(
        error.kind(),
        redis::ErrorKind::TypeError
            | redis::ErrorKind::ClientError
            | redis::ErrorKind::InvalidClientConfig
    ) {
        return false;
    }
    convert_redis_error_ref(error).is_unavailability()
}

fn convert_redis_error_ref(error: &redis::RedisError) -> CacheError {
    if error.is_timeout() {
        return CacheError::Timeout;
    }
    match error.kind() {
        redis::ErrorKind::IoError => {
            if error.to_string().contains("circuit breaker") {
                CacheError::CircuitBreakerOpen
            } else {
                CacheError::ConnectionFailed(error.to_string())
            }
        }
        _ => CacheError::ConnectionFailed(error.to_string()),
    }
}
    }
}

/// Result type for cache operations
pub type CacheResult<T> = Result<T, CacheError>;

/// Converts redis::RedisError to CacheError with appropriate handling
pub fn convert_redis_error(error: redis::RedisError) -> CacheError {
    match error.kind() {
        redis::ErrorKind::IoError => CacheError::ConnectionFailed(format!("I/O error: {}", error)),
        redis::ErrorKind::TypeError => {
            if error.to_string().contains("deserialization") {
                CacheError::SerializationError(error.to_string())
            } else {
                CacheError::ValidationError(error.to_string())
            }
        }
        redis::ErrorKind::ResponseError => {
            if error.to_string().contains("Circuit") {
                CacheError::CircuitBreakerOpen
            } else {
                CacheError::ConnectionFailed(error.to_string())
            }
        }
        _ => CacheError::ConnectionFailed(format!("Redis error: {}", error)),
    }
}

/// Emits the shared "degraded mode" signal for a Redis-dependent component.
///
/// Every Redis-dependent path (idempotency, rate limiting, query caching, ...)
/// calls this with its own `component` name when it falls back, so a single
/// metric/log pattern reveals the full blast radius of a Redis outage.
///
/// Returns `true` if the error was degradable (i.e. the caller should proceed
/// with its fallback); returns `false` for non-degradable errors so callers can
/// still surface genuine failures.
pub fn emit_degraded_mode(component: &str, error: &CacheError) -> bool {
    if !error.is_degradable() {
        return false;
    }

    // Shared, consistent signal: one metric name and one log shape for every
    // component, keyed by `component` and `reason`.
    tracing::warn!(
        metric = "cache_degraded_mode_total",
        component = component,
        reason = error.degraded_reason(),
        error = %error,
        "Redis cache unavailable; entering degraded mode"
    );

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_error_display() {
        let err = CacheError::ConnectionFailed("test error".to_string());
        assert!(err.to_string().contains("connection failed"));

        let err = CacheError::KeyNotFound;
        assert!(err.to_string().contains("not found"));

        let err = CacheError::SerializationError("test".to_string());
        assert!(err.to_string().contains("Serialization"));

        let err = CacheError::Timeout;
        assert!(err.to_string().contains("timed out"));

        let err = CacheError::CircuitBreakerOpen;
        assert!(err.to_string().contains("circuit breaker"));
    }

    #[test]
    fn test_key_not_found_is_not_fatal() {
        let err = CacheError::KeyNotFound;
        // KeyNotFound represents a cache miss, not an error condition
        assert_eq!(err.to_string(), "Key not found in cache");
    }

    #[test]
    fn unavailability_classification() {
        assert!(CacheError::ConnectionFailed("x".into()).is_unavailability());
        assert!(CacheError::Timeout.is_unavailability());
        assert!(CacheError::CircuitBreakerOpen.is_unavailability());
        assert!(!CacheError::KeyNotFound.is_unavailability());
        assert!(!CacheError::ValidationError("x".into()).is_unavailability());
        assert!(!CacheError::SerializationError("x".into()).is_unavailability());
    }

    #[test]
    fn raw_redis_error_unavailability() {
        let io = redis::RedisError::from(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "refused",
        ));
        assert!(is_redis_unavailable(&io));
        let open =
            redis::RedisError::from((redis::ErrorKind::IoError, "Redis circuit breaker is open"));
        assert!(is_redis_unavailable(&open));
        let validation =
            redis::RedisError::from((redis::ErrorKind::TypeError, "cache validation failed"));
        assert!(!is_redis_unavailable(&validation));
    }

    #[test]
    fn test_connection_failed_error() {
        let err = CacheError::ConnectionFailed("Connection refused".to_string());
        assert!(err.to_string().contains("Connection refused"));
    }

    #[test]
    fn test_degradable_errors_are_classified() {
        // Redis-unavailability errors must be degradable so callers fall back.
        assert!(CacheError::ConnectionFailed("down".to_string()).is_degradable());
        assert!(CacheError::Timeout.is_degradable());
        assert!(CacheError::CircuitBreakerOpen.is_degradable());

        // A cache miss and caller bugs are not infrastructure outages.
        assert!(!CacheError::KeyNotFound.is_degradable());
        assert!(!CacheError::SerializationError("bad".to_string()).is_degradable());
        assert!(!CacheError::ValidationError("bad".to_string()).is_degradable());
    }

    #[test]
    fn test_degraded_reason_labels_are_stable() {
        assert_eq!(
            CacheError::ConnectionFailed("x".to_string()).degraded_reason(),
            "connection_failed"
        );
        assert_eq!(CacheError::Timeout.degraded_reason(), "timeout");
        assert_eq!(
            CacheError::CircuitBreakerOpen.degraded_reason(),
            "circuit_breaker_open"
        );
        assert_eq!(CacheError::KeyNotFound.degraded_reason(), "cache_miss");
    }

    #[test]
    fn test_emit_degraded_mode_signals_only_degradable_errors() {
        // Degradable errors emit the shared signal and tell the caller to fall back.
        assert!(emit_degraded_mode(
            "query_cache",
            &CacheError::ConnectionFailed("down".to_string())
        ));
        assert!(emit_degraded_mode("rate_limiting", &CacheError::Timeout));

        // Non-degradable errors do not emit the degraded-mode signal.
        assert!(!emit_degraded_mode("query_cache", &CacheError::KeyNotFound));
        assert!(!emit_degraded_mode(
            "rate_limiting",
            &CacheError::ValidationError("bad".to_string())
        ));
    }
}
