/// Error types for the Auth module (vaultrs integration).
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("Invalid credentials: {0}")]
    InvalidCredentials(String),

    #[error("Token expired")]
    TokenExpired,

    #[error("Insufficient permissions: {0}")]
    InsufficientPermissions(String),

    #[error("Vault error: {0}")]
    Vault(String),

    #[error("Validation error: {0}")]
    Validation(String),
}

impl AuthError {
    /// HTTP status code for this error.
    pub fn status_code(&self) -> u16 {
        match self {
            AuthError::InvalidCredentials(_) | AuthError::TokenExpired => 401,
            AuthError::InsufficientPermissions(_) => 403,
            AuthError::Validation(_) => 400,
            AuthError::Vault(_) => 502,
        }
    }

    /// Stable error code for programmatic handling.
    pub fn code(&self) -> &'static str {
        match self {
            AuthError::InvalidCredentials(_) => "ERR_AUTH_001",
            AuthError::TokenExpired => "ERR_AUTH_002",
            AuthError::InsufficientPermissions(_) => "ERR_AUTH_003",
            AuthError::Vault(_) => "ERR_AUTH_004",
            AuthError::Validation(_) => "ERR_AUTH_005",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn test_display_messages() {
        assert!(AuthError::InvalidCredentials("x".into()).to_string().contains("Invalid credentials"));
        assert!(AuthError::TokenExpired.to_string().contains("expired"));
        assert!(AuthError::Vault("err".into()).to_string().contains("Vault"));
    }
}
