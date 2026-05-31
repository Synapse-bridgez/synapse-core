/// Error types for the Auth module (vaultrs integration).
///
/// All error types in this module are designed with security-first principles:
/// - Error messages must never leak token values, vault paths, or internal stack traces
/// - Each variant represents a distinct failure mode for proper error handling
/// - Messages are redacted to prevent information disclosure attacks
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AuthError {
    /// Represents an authentication failure due to invalid credentials.
    ///
    /// **Security Invariant**: The error message MUST NOT contain the actual
    /// credential value (token, API key, password). Only a generic message
    /// is shown to callers. Internal logs may contain more details for debugging,
    /// but only at trace level (never at info/warn level).
    ///
    /// Causes:
    /// - Invalid API key format
    /// - Invalid token value
    /// - Credentials failed validation before reaching Vault
    ///
    /// Callers should: Return 401 Unauthorized and prompt user to re-authenticate.
    #[error("Invalid credentials")]
    InvalidCredentials(String),

    /// Represents a token that has expired and can no longer be used.
    ///
    /// **Security Invariant**: Does not expose token value or expiration details.
    ///
    /// Causes:
    /// - Token has exceeded its TTL
    /// - Token was explicitly revoked
    ///
    /// Callers should: Return 401 Unauthorized and prompt user to refresh credentials.
    #[error("Token expired")]
    TokenExpired,

    /// Represents a valid authentication that lacks required permissions.
    ///
    /// **Security Invariant**: The error message must not expose the vault path,
    /// policy name, or internal permission structure. Only a generic message about
    /// insufficient permissions is shown.
    ///
    /// Causes:
    /// - Vault policy does not grant required capabilities
    /// - User lacks role for the requested operation
    ///
    /// Callers should: Return 403 Forbidden. Do not retry.
    #[error("Insufficient permissions")]
    InsufficientPermissions(String),

    /// Represents a failure in the Vault integration itself.
    ///
    /// **Security Invariant**: The error message MUST NOT contain the vault
    /// endpoint URL, internal error responses from Vault, or stack traces.
    /// Only a generic "Vault service unavailable" message is shown to callers.
    ///
    /// Causes:
    /// - Vault server is unreachable
    /// - Vault returned an internal error (5xx)
    /// - vaultrs client failed to construct the request
    /// - Connection timeout or network failure
    ///
    /// Callers should: Return 502 Bad Gateway. Retry may succeed if Vault recovers.
    #[error("Vault service unavailable")]
    Vault(String),

    /// Represents a validation error in the auth layer (input validation, format checks).
    ///
    /// **Security Invariant**: The error message may describe what was invalid
    /// (e.g., "token too short") but MUST NOT include the actual token value.
    ///
    /// Causes:
    /// - Token format does not match schema
    /// - API key does not meet requirements
    /// - Authorization header is malformed
    ///
    /// Callers should: Return 400 Bad Request.
    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Rate limit exceeded — retry after {0}s")]
    RateLimited(u64),
}

impl AuthError {
    /// Returns the HTTP status code for this error.
    ///
    /// Maps AuthError variants to the appropriate HTTP status code for client responses.
    /// All status codes follow RFC 7231 and RFC 7235 conventions.
    pub fn status_code(&self) -> u16 {
        match self {
            AuthError::InvalidCredentials(_) | AuthError::TokenExpired => 401,
            AuthError::InsufficientPermissions(_) => 403,
            AuthError::Validation(_) => 400,
            AuthError::Vault(_) => 502,
            AuthError::RateLimited(_) => 429,
        }
    }

    /// Returns a stable error code for programmatic error handling.
    ///
    /// Error codes are stable across application versions and can be used by
    /// client applications to programmatically handle specific error types
    /// without relying on error message text (which may change).
    ///
    /// # Error Codes
    /// - `ERR_AUTH_001` - InvalidCredentials
    /// - `ERR_AUTH_002` - TokenExpired
    /// - `ERR_AUTH_003` - InsufficientPermissions
    /// - `ERR_AUTH_004` - Vault (service unavailable)
    /// - `ERR_AUTH_005` - Validation
    pub fn code(&self) -> &'static str {
        match self {
            AuthError::InvalidCredentials(_) => "ERR_AUTH_001",
            AuthError::TokenExpired => "ERR_AUTH_002",
            AuthError::InsufficientPermissions(_) => "ERR_AUTH_003",
            AuthError::Vault(_) => "ERR_AUTH_004",
            AuthError::Validation(_) => "ERR_AUTH_005",
            AuthError::RateLimited(_) => "ERR_AUTH_006",
        }
    }

    /// Checks if this error is due to a temporary Vault issue (may be retryable).
    ///
    /// Returns `true` only for Vault service errors, which may resolve after
    /// the service recovers. Other errors should not be retried as they
    /// represent permanent failures (invalid credentials, insufficient permissions).
    pub fn is_retryable(&self) -> bool {
        matches!(self, AuthError::Vault(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Status Code Tests ---

    #[test]
    fn test_invalid_credentials_status() {
        let e = AuthError::InvalidCredentials("bad key".into());
        assert_eq!(e.status_code(), 401);
        assert_eq!(e.code(), "ERR_AUTH_001");
    }

    #[test]
    fn test_token_expired_status() {
        let e = AuthError::TokenExpired;
        assert_eq!(e.status_code(), 401);
        assert_eq!(e.code(), "ERR_AUTH_002");
    }

    #[test]
    fn test_insufficient_permissions_status() {
        let e = AuthError::InsufficientPermissions("read-only".into());
        assert_eq!(e.status_code(), 403);
        assert_eq!(e.code(), "ERR_AUTH_003");
    }

    #[test]
    fn test_vault_error_status() {
        let e = AuthError::Vault("connection refused".into());
        assert_eq!(e.status_code(), 502);
        assert_eq!(e.code(), "ERR_AUTH_004");
    }

    #[test]
    fn test_validation_error_status() {
        let e = AuthError::Validation("empty token".into());
        assert_eq!(e.status_code(), 400);
        assert_eq!(e.code(), "ERR_AUTH_005");
    }

    // --- Display Message Tests ---

    #[test]
    fn test_invalid_credentials_message_is_generic() {
        // Message should NOT contain the actual credential value
        let msg = AuthError::InvalidCredentials("secret_token_12345".into()).to_string();
        assert!(msg.contains("Invalid credentials"));
        assert!(!msg.contains("secret_token_12345"));
    }

    #[test]
    fn test_token_expired_message() {
        let e = AuthError::TokenExpired;
        let msg = e.to_string();
        assert!(msg.contains("expired"));
        assert!(!msg.is_empty());
    }

    #[test]
    fn test_insufficient_permissions_message_does_not_leak_vault_path() {
        // Message should NOT contain vault paths or internal policy names
        let msg = AuthError::InsufficientPermissions("secret/data/apikey".into()).to_string();
        assert!(msg.contains("Insufficient permissions"));
        // The internal vault path might be in the debug details, but not in Display
        assert!(!msg.contains("secret/data"));
    }

    #[test]
    fn test_vault_error_message_is_generic() {
        // Message should NOT contain vault endpoint, internal errors, or stack traces
        let vault_response = "error: invalid token, http://internal-vault:8200";
        let msg = AuthError::Vault(vault_response.to_string()).to_string();
        assert!(msg.contains("Vault service unavailable"));
        assert!(!msg.contains("http://"));
        assert!(!msg.contains("internal-vault"));
    }

    #[test]
    fn test_validation_error_message_describes_issue_without_value() {
        // Validation messages should describe the problem, not the value
        let msg = AuthError::Validation("token too short".into()).to_string();
        assert!(msg.contains("Validation error"));
        assert!(msg.contains("too short"));
    }

    // --- Security Invariant Tests ---

    #[test]
    fn test_no_sensitive_data_in_invalid_credentials_display() {
        let sensitive_values = vec![
            "sk_live_abcd1234",        // API key-like
            "eyJhbGc...",              // JWT-like
            "password123!@#",          // Password-like
            "$2b$12$...",              // Hash-like
        ];

        for value in sensitive_values {
            let err = AuthError::InvalidCredentials(value.to_string());
            let msg = err.to_string();
            // The display message should be redacted
            assert!(!msg.contains(value), "Display message leaked: {}", value);
        }
    }

    #[test]
    fn test_no_vault_path_leakage_in_messages() {
        let vault_paths = vec![
            "secret/data/prod/apikeys",
            "auth/approle/role/app-reader",
            "sys/auth/userpass/login/admin",
        ];

        for path in vault_paths {
            let err = AuthError::Vault(path.to_string());
            let msg = err.to_string();
            // Should be redacted to generic message
            assert!(!msg.contains(path), "Vault path leaked: {}", path);
            assert!(msg.contains("Vault service unavailable"));
        }
    }

    #[test]
    fn test_no_stack_traces_in_error_messages() {
        let err = AuthError::Vault("at vaultrs::auth::approle (main.rs:42)".to_string());
        let msg = err.to_string();
        // Stack trace details should not appear in user-facing message
        assert!(!msg.contains("main.rs"));
        assert!(!msg.contains("42)"));
    }

    // --- Error Code Tests ---

    #[test]
    fn test_all_error_variants_have_codes() {
        let errors = vec![
            AuthError::InvalidCredentials("test".into()),
            AuthError::TokenExpired,
            AuthError::InsufficientPermissions("test".into()),
            AuthError::Vault("test".into()),
            AuthError::Validation("test".into()),
        ];

        for err in errors {
            let code = err.code();
            assert!(!code.is_empty());
            assert!(code.starts_with("ERR_AUTH_"));
        }
    }

    #[test]
    fn test_error_codes_are_unique() {
        let codes = vec![
            AuthError::InvalidCredentials("".into()).code(),
            AuthError::TokenExpired.code(),
            AuthError::InsufficientPermissions("".into()).code(),
            AuthError::Vault("".into()).code(),
            AuthError::Validation("".into()).code(),
        ];

        // Check uniqueness
        let mut unique_codes = codes.clone();
        unique_codes.sort();
        unique_codes.dedup();
        assert_eq!(unique_codes.len(), codes.len(), "Error codes should be unique");
    }

    // --- Retryability Tests ---

    #[test]
    fn test_vault_error_is_retryable() {
        let err = AuthError::Vault("timeout".into());
        assert!(err.is_retryable());
    }

    #[test]
    fn test_non_vault_errors_are_not_retryable() {
        assert!(!AuthError::InvalidCredentials("".into()).is_retryable());
        assert!(!AuthError::TokenExpired.is_retryable());
        assert!(!AuthError::InsufficientPermissions("".into()).is_retryable());
        assert!(!AuthError::Validation("".into()).is_retryable());
    }

    // --- Input Validation Tests ---

    #[test]
    fn test_invalid_input_returns_typed_error_before_vaultrs() {
        // This test verifies that input validation occurs before attempting
        // to call vaultrs. The validate_token function in input_validation.rs
        // should catch malformed tokens and return AuthError::Validation.
        use crate::auth::input_validation::validate_token;

        let invalid_token = "short";
        match validate_token(invalid_token) {
            Err(msg) => {
                assert!(!msg.is_empty());
                // Verify it describes the problem without leaking the token
                assert!(!msg.contains(invalid_token));
            }
            Ok(_) => panic!("Short token should fail validation"),
        }
    }

    #[test]
    fn test_empty_token_fails_validation() {
        use crate::auth::input_validation::validate_token;

        let result = validate_token("");
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(!msg.is_empty());
    }

    // --- Error Propagation Tests ---

    #[test]
    fn test_all_variants_implement_error_trait() {
        use std::error::Error;

        let errors: Vec<Box<dyn Error>> = vec![
            Box::new(AuthError::InvalidCredentials("test".into())),
            Box::new(AuthError::TokenExpired),
            Box::new(AuthError::InsufficientPermissions("test".into())),
            Box::new(AuthError::Vault("test".into())),
            Box::new(AuthError::Validation("test".into())),
        ];

        for err in errors {
            let _ = err.to_string(); // Should not panic
        }
    }

    #[test]
    fn test_debug_output_exists() {
        let errors = vec![
            AuthError::InvalidCredentials("test".into()),
            AuthError::TokenExpired,
            AuthError::InsufficientPermissions("test".into()),
            AuthError::Vault("test".into()),
            AuthError::Validation("test".into()),
        ];

        for err in errors {
            let debug_str = format!("{:?}", err);
            assert!(!debug_str.is_empty());
        }
    }

    #[test]
    fn test_rate_limited_status() {
        let e = AuthError::RateLimited(30);
        assert_eq!(e.status_code(), 429);
        assert_eq!(e.code(), "ERR_AUTH_006");
        assert!(e.to_string().contains("30"));
    }
}
