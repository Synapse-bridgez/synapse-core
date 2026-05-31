//! Secure session management with validation and session checks.
//!
//! This module provides session validation logic that acts as a health check
//! for the security layer. Invalid sessions indicate a security issue or misconfiguration.

use chrono::{DateTime, Utc};
use uuid::Uuid;

const MAX_SESSION_TTL_SECS: i64 = 86_400; // 24 h
const MAX_USER_ID_LEN: usize = 128;

/// Errors that can occur during session validation.
///
/// These errors indicate either invalid input (misconfiguration) or session expiration/inactivity
/// (degraded security state). All errors are non-fatal; callers should handle gracefully.
#[derive(Debug, thiserror::Error)]
pub enum SessionValidationError {
    /// User ID is empty. Indicates misconfiguration at session creation time.
    #[error("User ID is empty")]
    EmptyUserId,

    /// User ID exceeds maximum length (128 chars). Indicates input validation failure.
    #[error("User ID exceeds maximum length")]
    UserIdTooLong,

    /// TTL is invalid. Must be between 1 and 86400 seconds (24 hours).
    /// Indicates misconfiguration or request tampering.
    #[error("TTL must be between 1 and {MAX_SESSION_TTL_SECS} seconds")]
    InvalidTtl,

    /// Session has expired. The session is stale and should not be trusted.
    /// Caller should prompt for re-authentication.
    #[error("Session has expired")]
    Expired,

    /// Session is inactive. The session was explicitly deactivated/revoked.
    /// Caller should deny access and require re-authentication.
    #[error("Session is inactive")]
    Inactive,
}

/// A session record that can be validated for liveness and integrity.
///
/// This struct serves as a health check for the session layer. Validation determines
/// whether the session is still valid, active, and within TTL bounds.
///
/// # Fields
/// - `id`: Unique session identifier
/// - `user_id`: Associated user; must be non-empty and ≤128 chars
/// - `expires_at`: Expiration time; if in the past, session is stale
/// - `is_active`: Flag indicating whether the session has been explicitly revoked
#[derive(Debug, Clone)]
pub struct SessionRecord {
    /// Unique session identifier
    pub id: Uuid,
    /// Associated user ID (must be non-empty)
    pub user_id: String,
    /// Absolute time when this session expires
    pub expires_at: DateTime<Utc>,
    /// Whether the session is active (false = revoked)
    pub is_active: bool,
}

/// Validates input parameters before creating a new session.
///
/// This function acts as a health check for session creation configuration.
/// It ensures user ID and TTL are within acceptable bounds.
///
/// # Arguments
/// - `user_id`: User identifier; must be non-empty and ≤128 characters
/// - `ttl_seconds`: Session time-to-live; must be between 1 and 86400 seconds
///
/// # Returns
/// - `Ok(())` if inputs are valid
/// - `Err(SessionValidationError)` if user_id is empty, too long, or ttl_seconds is out of range
///
/// # Caller responsibility
/// If validation fails, reject the session creation request and log the error.
/// Do not attempt to create a session with invalid parameters.
pub fn validate_session_params(user_id: &str, ttl_seconds: i64) -> Result<(), SessionValidationError> {
    if user_id.is_empty() {
        return Err(SessionValidationError::EmptyUserId);
    }
    if user_id.len() > MAX_USER_ID_LEN {
        return Err(SessionValidationError::UserIdTooLong);
    }
    if !(1..=MAX_SESSION_TTL_SECS).contains(&ttl_seconds) {
        return Err(SessionValidationError::InvalidTtl);
    }
    Ok(())
}

/// Validates that an existing session is still active, usable, and within TTL.
///
/// This is the primary health check for the session layer. It determines whether
/// a session can be trusted for authorization decisions. A valid session must:
/// - Be marked as active (not revoked)
/// - Not have expired (expiration time must be in the future)
///
/// # Arguments
/// - `session`: The session record to validate
///
/// # Returns
/// - `Ok(())` if the session is healthy (active and not expired)
/// - `Err(SessionValidationError::Inactive)` if the session has been revoked
/// - `Err(SessionValidationError::Expired)` if the current time is at or past `expires_at`
///
/// # Caller responsibility
/// - If validation fails with `Inactive`, deny access immediately (session was revoked)
/// - If validation fails with `Expired`, treat as stale session and prompt re-authentication
/// - Callers must check this before granting access to protected resources
///
/// # Non-fatal behavior
/// This function is non-fatal; its job is to report the security state so callers
/// can make appropriate decisions. The overall system continues even if sessions are invalid.
pub fn validate_session(session: &SessionRecord) -> Result<(), SessionValidationError> {
    if !session.is_active {
        return Err(SessionValidationError::Inactive);
    }
    if session.expires_at <= Utc::now() {
        return Err(SessionValidationError::Expired);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn test_valid_params() {
        assert!(validate_session_params("user123", 3600).is_ok());
    }

    #[test]
    fn test_empty_user_id() {
        assert!(matches!(
            validate_session_params("", 3600),
            Err(SessionValidationError::EmptyUserId)
        ));
    }

    #[test]
    fn test_user_id_too_long() {
        let long_id = "a".repeat(MAX_USER_ID_LEN + 1);
        assert!(matches!(
            validate_session_params(&long_id, 3600),
            Err(SessionValidationError::UserIdTooLong)
        ));
    }

    #[test]
    fn test_invalid_ttl_zero() {
        assert!(matches!(
            validate_session_params("user", 0),
            Err(SessionValidationError::InvalidTtl)
        ));
    }

    #[test]
    fn test_invalid_ttl_negative() {
        assert!(matches!(
            validate_session_params("user", -1),
            Err(SessionValidationError::InvalidTtl)
        ));
    }

    #[test]
    fn test_invalid_ttl_exceeds_max() {
        assert!(matches!(
            validate_session_params("user", MAX_SESSION_TTL_SECS + 1),
            Err(SessionValidationError::InvalidTtl)
        ));
    }

    #[test]
    fn test_valid_session() {
        let session = SessionRecord {
            id: Uuid::new_v4(),
            user_id: "user1".into(),
            expires_at: Utc::now() + Duration::hours(1),
            is_active: true,
        };
        assert!(validate_session(&session).is_ok());
    }

    #[test]
    fn test_expired_session() {
        let session = SessionRecord {
            id: Uuid::new_v4(),
            user_id: "user1".into(),
            expires_at: Utc::now() - Duration::seconds(1),
            is_active: true,
        };
        assert!(matches!(
            validate_session(&session),
            Err(SessionValidationError::Expired)
        ));
    }

    #[test]
    fn test_inactive_session() {
        let session = SessionRecord {
            id: Uuid::new_v4(),
            user_id: "user1".into(),
            expires_at: Utc::now() + Duration::hours(1),
            is_active: false,
        };
        assert!(matches!(
            validate_session(&session),
            Err(SessionValidationError::Inactive)
        ));
    }
}
