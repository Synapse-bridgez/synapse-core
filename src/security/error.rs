//! Error types for the Security module.
//!
//! [`SecurityError`] is the single error type returned by security checks in
//! this module — rate limiting ([`RateLimiter`](crate::cache::rate_limiting::RateLimiter))
//! and session validation ([`validate_session`](crate::security::session::validate_session),
//! [`validate_session_params`](crate::security::session::validate_session_params)).
//!
//! # Error categories
//!
//! | Variant | HTTP | Stable code |
//! |---------|------|-------------|
//! | [`RateLimitExceeded`](SecurityError::RateLimitExceeded) | 429 | `ERR_SECURITY_001` |
//! | [`SessionValidation`](SecurityError::SessionValidation) | 400 | `ERR_SECURITY_002` |
//!
//! # Security notes
//!
//! - `RateLimitExceeded` intentionally carries no internal state (token counts,
//!   bucket configuration) to avoid leaking rate-limit parameters to callers.
//! - `SessionValidation` wraps [`SessionValidationError`] whose `Display` impl
//!   is safe to surface to clients — it never includes raw database values or
//!   internal identifiers.
//!
//! # Example
//!
//! ```rust
//! use synapse_core::security::error::SecurityError;
//! use synapse_core::security::session::{validate_session_params, SessionValidationError};
//!
//! fn check(user_id: &str, ttl: i64) -> Result<(), SecurityError> {
//!     validate_session_params(user_id, ttl).map_err(SecurityError::SessionValidation)
//! }
//!
//! assert!(matches!(check("", 3600), Err(SecurityError::SessionValidation(SessionValidationError::EmptyUserId))));
//! ```

use thiserror::Error;

use crate::security::session::SessionValidationError;

/// Errors produced by security checks (rate limiting and session validation).
#[derive(Debug, Error)]
pub enum SecurityError {
    /// The caller has exceeded the allowed request rate.
    ///
    /// Callers should back off and retry after the window resets.  The HTTP
    /// layer maps this to **429 Too Many Requests**.
    #[error("Rate limit exceeded")]
    RateLimitExceeded,

    /// A session parameter or session record failed validation.
    ///
    /// The inner [`SessionValidationError`] provides a client-safe description
    /// of the specific constraint that was violated.  The HTTP layer maps this
    /// to **400 Bad Request**.
    #[error("Session validation failed: {0}")]
    SessionValidation(#[from] SessionValidationError),
}

impl SecurityError {
    /// HTTP status code for this error.
    pub fn status_code(&self) -> u16 {
        match self {
            SecurityError::RateLimitExceeded => 429,
            SecurityError::SessionValidation(_) => 400,
        }
    }

    /// Stable error code for programmatic handling.
    ///
    /// These codes are stable across releases and safe to include in API
    /// responses.  See `src/error.rs` for the full catalogue.
    pub fn code(&self) -> &'static str {
        match self {
            SecurityError::RateLimitExceeded => "ERR_SECURITY_001",
            SecurityError::SessionValidation(_) => "ERR_SECURITY_002",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_exceeded_status_and_code() {
        let e = SecurityError::RateLimitExceeded;
        assert_eq!(e.status_code(), 429);
        assert_eq!(e.code(), "ERR_SECURITY_001");
        assert_eq!(e.to_string(), "Rate limit exceeded");
    }

    #[test]
    fn session_validation_status_and_code() {
        let e = SecurityError::SessionValidation(SessionValidationError::EmptyUserId);
        assert_eq!(e.status_code(), 400);
        assert_eq!(e.code(), "ERR_SECURITY_002");
        assert!(e.to_string().contains("Session validation failed"));
    }

    #[test]
    fn session_validation_from_impl() {
        let inner = SessionValidationError::InvalidTtl;
        let e: SecurityError = inner.into();
        assert_eq!(e.status_code(), 400);
    }

    #[test]
    fn session_validation_display_includes_inner() {
        let e = SecurityError::SessionValidation(SessionValidationError::Expired);
        assert!(e.to_string().contains("expired") || e.to_string().contains("Session validation failed"));
    }
}
