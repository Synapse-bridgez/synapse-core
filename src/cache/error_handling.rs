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
    fn test_connection_failed_error() {
        let err = CacheError::ConnectionFailed("Connection refused".to_string());
        assert!(err.to_string().contains("Connection refused"));
    }
}
