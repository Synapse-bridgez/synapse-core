//! Secure error handling for WebSocket connections
//!
//! This module provides validation and security checks for WebSocket error handling,
//! ensuring that sensitive information is not leaked to clients and that all errors
//! are properly logged and categorized.

use serde::{Deserialize, Serialize};
use std::fmt;

/// WebSocket-specific error types with security considerations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WsError {
    /// Authentication failed - safe to expose to client
    AuthenticationFailed,
    /// Invalid message format - safe to expose to client
    InvalidMessageFormat,
    /// Message limit exceeded - safe to expose to client
    MessageLimitExceeded,
    /// Connection timeout - safe to expose to client
    ConnectionTimeout,
    /// Internal server error - should NOT expose details to client
    InternalError,
    /// Database error - should NOT expose details to client
    DatabaseError,
    /// Serialization error - should NOT expose details to client
    SerializationError,
}

impl fmt::Display for WsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WsError::AuthenticationFailed => write!(f, "Authentication failed"),
            WsError::InvalidMessageFormat => write!(f, "Invalid message format"),
            WsError::MessageLimitExceeded => write!(f, "Message limit exceeded"),
            WsError::ConnectionTimeout => write!(f, "Connection timeout"),
            WsError::InternalError => write!(f, "Internal server error"),
            WsError::DatabaseError => write!(f, "Database error"),
            WsError::SerializationError => write!(f, "Serialization error"),
        }
    }
}

impl WsError {
    /// Get the client-safe error message
    ///
    /// Returns a message that is safe to send to the client without leaking
    /// sensitive information about the system.
    pub fn client_message(&self) -> &'static str {
        match self {
            WsError::AuthenticationFailed => "Authentication failed",
            WsError::InvalidMessageFormat => "Invalid message format",
            WsError::MessageLimitExceeded => "Message limit exceeded",
            WsError::ConnectionTimeout => "Connection timeout",
            // Internal errors should not expose details
            WsError::InternalError => "An error occurred",
            WsError::DatabaseError => "An error occurred",
            WsError::SerializationError => "An error occurred",
        }
    }

    /// Check if this error should be logged with full details
    pub fn should_log_details(&self) -> bool {
        matches!(
            self,
            WsError::InternalError | WsError::DatabaseError | WsError::SerializationError
        )
    }

    /// Check if this error is safe to expose to the client
    pub fn is_client_safe(&self) -> bool {
        !self.should_log_details()
    }
}

/// Validates WebSocket token format
///
/// # Security
/// - Rejects empty tokens
/// - Rejects tokens with suspicious patterns
/// - Validates token length
pub fn validate_ws_token(token: &str) -> Result<(), WsError> {
    // Reject empty tokens
    if token.is_empty() {
        return Err(WsError::AuthenticationFailed);
    }

    // Reject tokens that are too long (potential DoS)
    if token.len() > 1024 {
        return Err(WsError::AuthenticationFailed);
    }

    // Reject tokens with null bytes (potential injection)
    if token.contains('\0') {
        return Err(WsError::AuthenticationFailed);
    }

    Ok(())
}

/// Validates client message size to prevent DoS attacks
///
/// # Security
/// - Enforces maximum message size
/// - Prevents memory exhaustion attacks
pub fn validate_message_size(message: &str) -> Result<(), WsError> {
    const MAX_MESSAGE_SIZE: usize = 1024 * 1024; // 1MB

    if message.len() > MAX_MESSAGE_SIZE {
        return Err(WsError::MessageLimitExceeded);
    }

    Ok(())
}

/// Validates JSON message structure
///
/// # Security
/// - Ensures message is valid JSON
/// - Prevents malformed message attacks
pub fn validate_message_structure(message: &str) -> Result<serde_json::Value, WsError> {
    serde_json::from_str(message).map_err(|_| WsError::InvalidMessageFormat)
}

/// Validates resync limit parameter
///
/// # Security
/// - Enforces minimum limit (prevents empty results)
/// - Enforces maximum limit (prevents resource exhaustion)
pub fn validate_resync_limit(limit: Option<i64>) -> Result<i64, WsError> {
    const MIN_LIMIT: i64 = 1;
    const MAX_LIMIT: i64 = 100;

    let limit = limit.unwrap_or(20);

    if !(MIN_LIMIT..=MAX_LIMIT).contains(&limit) {
        return Err(WsError::InvalidMessageFormat);
    }

    Ok(limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_ws_token_empty() {
        assert!(validate_ws_token("").is_err());
    }

    #[test]
    fn test_validate_ws_token_valid() {
        assert!(validate_ws_token("valid_token_123").is_ok());
    }

    #[test]
    fn test_validate_ws_token_too_long() {
        let long_token = "a".repeat(2000);
        assert!(validate_ws_token(&long_token).is_err());
    }

    #[test]
    fn test_validate_ws_token_null_byte() {
        assert!(validate_ws_token("token\0injection").is_err());
    }

    #[test]
    fn test_validate_message_size_valid() {
        let msg = "x".repeat(1000);
        assert!(validate_message_size(&msg).is_ok());
    }

    #[test]
    fn test_validate_message_size_too_large() {
        let msg = "x".repeat(2 * 1024 * 1024);
        assert!(validate_message_size(&msg).is_err());
    }

    #[test]
    fn test_validate_message_structure_valid() {
        let result = validate_message_structure(r#"{"type": "resync"}"#);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_message_structure_invalid() {
        let result = validate_message_structure("not json");
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_resync_limit_default() {
        let limit = validate_resync_limit(None).unwrap();
        assert_eq!(limit, 20);
    }

    #[test]
    fn test_validate_resync_limit_valid() {
        let limit = validate_resync_limit(Some(50)).unwrap();
        assert_eq!(limit, 50);
    }

    #[test]
    fn test_validate_resync_limit_too_small() {
        assert!(validate_resync_limit(Some(0)).is_err());
    }

    #[test]
    fn test_validate_resync_limit_too_large() {
        assert!(validate_resync_limit(Some(200)).is_err());
    }

    #[test]
    fn test_ws_error_client_message_safe() {
        assert_eq!(
            WsError::AuthenticationFailed.client_message(),
            "Authentication failed"
        );
    }

    #[test]
    fn test_ws_error_client_message_internal() {
        assert_eq!(WsError::InternalError.client_message(), "An error occurred");
    }

    #[test]
    fn test_ws_error_should_log_details() {
        assert!(WsError::InternalError.should_log_details());
        assert!(!WsError::AuthenticationFailed.should_log_details());
    }

    #[test]
    fn test_ws_error_is_client_safe() {
        assert!(WsError::AuthenticationFailed.is_client_safe());
        assert!(!WsError::InternalError.is_client_safe());
    }
}
