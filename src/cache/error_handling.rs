//! Error handling for cache operations.
//!
//! Provides structured error types for Redis cache operations with support for
//! distinguishing between cache misses (not an error) and actual errors.

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
    /// True when the error means Redis itself is unreachable or refusing
    /// work (connection failure, timeout, open breaker) — the cases a caller
    /// must degrade around — as opposed to a miss or a bad input.
    pub fn is_unavailability(&self) -> bool {
        matches!(
            self,
            CacheError::ConnectionFailed(_) | CacheError::Timeout | CacheError::CircuitBreakerOpen
        )
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
}
