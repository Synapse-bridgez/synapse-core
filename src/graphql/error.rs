//! Centralized error handling for the GraphQL module.
//!
//! Provides a typed [`GraphQlError`] enum that maps domain errors to structured
//! [`async_graphql::Error`] values with stable `extensions.code` fields.
//!
//! # Design
//!
//! - All GraphQL-layer errors carry a stable machine-readable `code` in the
//!   `extensions` object so clients can branch on error type without parsing
//!   the human-readable `message`.
//! - Internal details (database messages, stack traces) are never forwarded to
//!   the client; they are logged server-side via `tracing`.
//! - The conversion from [`GraphQlError`] to [`async_graphql::Error`] is
//!   zero-allocation for the common path: the `message` string is built once
//!   and the extension map is populated inline.
//!
//! # Stable error codes
//!
//! | Code | Meaning |
//! |------|---------|
//! | `VALIDATION_ERROR` | Input field failed validation |
//! | `NOT_FOUND` | Requested resource does not exist |
//! | `AUTHENTICATION_ERROR` | Request lacks valid credentials |
//! | `AUTHORIZATION_ERROR` | Caller lacks permission for the resource |
//! | `RATE_LIMITED` | Caller has exceeded the request rate limit |
//! | `COMPLEXITY_ERROR` | Query exceeds depth / complexity / alias limits |
//! | `DATABASE_ERROR` | Underlying database operation failed (details redacted) |
//! | `INTERNAL_ERROR` | Unexpected server error (details redacted) |

use async_graphql::Error as GqlError;
use async_graphql::ErrorExtensions;

// ---------------------------------------------------------------------------
// Stable error codes
// ---------------------------------------------------------------------------

///
/// Codes and messages mirror `docs/error-catalog.md`; they are stable and must
/// never be renamed or reused for a different error.
pub const CODE_VALIDATION: &str = "ERR_VALIDATION_001";
pub const CODE_NOT_FOUND: &str = "ERR_NOT_FOUND_001";
pub const CODE_AUTHENTICATION: &str = "ERR_AUTH_001";
pub const CODE_AUTHORIZATION: &str = "ERR_AUTH_002";
pub const CODE_RATE_LIMITED: &str = "ERR_RATE_LIMIT_001";
pub const CODE_COMPLEXITY: &str = "ERR_BAD_REQUEST_001";
pub const CODE_DATABASE: &str = "ERR_DATABASE_002";
pub const CODE_INTERNAL: &str = "ERR_INTERNAL_001";
pub const CODE_CONFLICT: &str = "ERR_TRANSACTION_004";

/// Catalog message for [`CODE_VALIDATION`].
pub const MSG_VALIDATION: &str = "Validation error - invalid input";
/// Catalog message for [`CODE_NOT_FOUND`].
pub const MSG_NOT_FOUND: &str = "Resource not found";
/// Catalog message for [`CODE_AUTHENTICATION`].
pub const MSG_AUTHENTICATION: &str = "Invalid authentication credentials";
/// Catalog message for [`CODE_AUTHORIZATION`].
pub const MSG_AUTHORIZATION: &str = "Insufficient permissions";
/// Catalog message for [`CODE_RATE_LIMITED`].
pub const MSG_RATE_LIMITED: &str = "Rate limit exceeded";
/// Catalog message for [`CODE_COMPLEXITY`].
pub const MSG_COMPLEXITY: &str = "Bad request - invalid parameters";
/// Catalog message for [`CODE_DATABASE`].
pub const MSG_DATABASE: &str = "Database query execution error";
/// Catalog message for [`CODE_INTERNAL`].
pub const MSG_INTERNAL: &str = "Internal server error";
/// Catalog message for [`CODE_CONFLICT`].
pub const MSG_CONFLICT: &str = "Transaction already processed (idempotency)";

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Typed errors produced by GraphQL resolvers and extensions.
///
/// Each variant carries only the information that is safe to surface to the
/// client.  Sensitive details (raw DB errors, internal state) must be logged
/// before constructing the error and must not be included in the variant payload.
#[derive(Debug)]
pub enum GraphQlError {
    /// An input field failed validation.
    Validation(String),

    /// The requested resource was not found.
    NotFound(String),

    /// The request lacks valid authentication credentials.
    Authentication,

    /// The authenticated caller lacks permission for the requested resource.
    Authorization,

    /// The caller has exceeded the configured request rate limit.
    ///
    /// `retry_after_secs` is a hint to the client; `0` means unknown.
    RateLimited { retry_after_secs: u64 },

    /// The query exceeds depth, complexity, or alias limits.
    Complexity(String),

    /// An underlying database operation failed.
    ///
    /// The `detail` string must be a generic, non-sensitive description.
    /// Log the raw error before constructing this variant.
    Database(String),

    /// An unexpected internal error occurred.
    ///
    /// The `detail` string must be a generic, non-sensitive description.
    /// Log the raw error before constructing this variant.
    Internal(String),

    /// The request conflicts with the current state of the resource
    /// (e.g. a duplicate submission caught by idempotency).
    ///
    /// The `detail` string must be a generic, non-sensitive description.
    Conflict(String),
}

impl GraphQlError {
    /// Returns the stable error code for this variant.
    pub fn code(&self) -> &'static str {
        match self {
            GraphQlError::Validation(_) => CODE_VALIDATION,
            GraphQlError::NotFound(_) => CODE_NOT_FOUND,
            GraphQlError::Authentication => CODE_AUTHENTICATION,
            GraphQlError::Authorization => CODE_AUTHORIZATION,
            GraphQlError::RateLimited { .. } => CODE_RATE_LIMITED,
            GraphQlError::Complexity(_) => CODE_COMPLEXITY,
            GraphQlError::Database(_) => CODE_DATABASE,
            GraphQlError::Internal(_) => CODE_INTERNAL,
            GraphQlError::Conflict(_) => CODE_CONFLICT,
        }
    }

    /// Converts this error into an [`async_graphql::Error`] with a populated
    /// `extensions.code` field (and `retryAfter` for rate-limit errors).
    ///
    /// Known variants map to the catalog code and message from
    /// `docs/error-catalog.md`. Unknown variants stay generic and the original
    /// error is logged server-side.
    pub fn into_gql_error(self) -> GqlError {
        let code = self.code();
        let message = self.message();

        if matches!(self, GraphQlError::Internal(_)) {
            tracing::error!(error = ?self, "GraphQL resolver: unmapped internal error");
        }

        let mut err = GqlError::new(message);
        err = err.extend_with(|_, e| e.set("code", code));

        if let GraphQlError::RateLimited { retry_after_secs } = &self {
            err = err.extend_with(|_, e| e.set("retryAfter", *retry_after_secs));
        }

        err
    }

    /// Returns the client-facing error message for this variant.
    ///
    /// Messages are the catalog messages; caller-supplied detail is appended
    /// only where it is safe to expose (validation, not-found, complexity).
    fn message(&self) -> String {
        match self {
            GraphQlError::Validation(msg) => format!("{}: {}", MSG_VALIDATION, msg),
            GraphQlError::NotFound(resource) => format!("{}: {}", MSG_NOT_FOUND, resource),
            GraphQlError::Authentication => MSG_AUTHENTICATION.to_string(),
            GraphQlError::Authorization => MSG_AUTHORIZATION.to_string(),
            GraphQlError::RateLimited { .. } => MSG_RATE_LIMITED.to_string(),
            GraphQlError::Complexity(msg) => format!("{}: {}", MSG_COMPLEXITY, msg),
            GraphQlError::Database(_) => MSG_DATABASE.to_string(),
            GraphQlError::Internal(_) => MSG_INTERNAL.to_string(),
            GraphQlError::Conflict(_) => MSG_CONFLICT.to_string(),
        }
    }
}

impl From<GraphQlError> for GqlError {
    fn from(e: GraphQlError) -> Self {
        e.into_gql_error()
    }
}

// ---------------------------------------------------------------------------
// Convenience constructors
// ---------------------------------------------------------------------------

/// Builds a validation error from a field name and reason.
pub fn validation_error(field: &str, reason: &str) -> GqlError {
    GraphQlError::Validation(format!("Invalid '{}': {}", field, reason)).into()
}

/// Builds a not-found error for a named resource.
pub fn not_found_error(resource: &str) -> GqlError {
    GraphQlError::NotFound(resource.to_string()).into()
}

/// Builds a conflict error (e.g. duplicate submission caught by idempotency).
pub fn conflict_error(detail: &str) -> GqlError {
    GraphQlError::Conflict(detail.to_string()).into()
}

/// Builds a database error, logging the raw cause server-side.
///
/// `raw_cause` is logged at `error` level and is never forwarded to the client.
pub fn database_error(raw_cause: &dyn std::fmt::Display) -> GqlError {
    tracing::error!(cause = %raw_cause, "GraphQL resolver: database error");
    GraphQlError::Database("Database operation failed".to_string()).into()
}

/// Builds an internal error, logging the raw cause server-side.
///
/// `raw_cause` is logged at `error` level and is never forwarded to the client.
pub fn internal_error(raw_cause: &dyn std::fmt::Display) -> GqlError {
    tracing::error!(cause = %raw_cause, "GraphQL resolver: internal error");
    GraphQlError::Internal("An internal error occurred".to_string()).into()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn ext_code(err: &GqlError) -> Option<String> {
        err.extensions
            .as_ref()
            .and_then(|e| e.get("code"))
            .and_then(|v| match v {
                async_graphql::Value::String(s) => Some(s.clone()),
                _ => None,
            })
    }

    #[test]
    fn validation_error_has_correct_code() {
        let err: GqlError = GraphQlError::Validation("bad input".into()).into();
        assert_eq!(ext_code(&err).as_deref(), Some(CODE_VALIDATION));
        assert!(err.message.contains("bad input"));
    }

    #[test]
    fn not_found_error_has_correct_code() {
        let err: GqlError = GraphQlError::NotFound("Transaction".into()).into();
        assert_eq!(ext_code(&err).as_deref(), Some(CODE_NOT_FOUND));
        assert!(err.message.contains("Transaction"));
    }

    #[test]
    fn authentication_error_has_correct_code() {
        let err: GqlError = GraphQlError::Authentication.into();
        assert_eq!(ext_code(&err).as_deref(), Some(CODE_AUTHENTICATION));
    }

    #[test]
    fn authorization_error_has_correct_code() {
        let err: GqlError = GraphQlError::Authorization.into();
        assert_eq!(ext_code(&err).as_deref(), Some(CODE_AUTHORIZATION));
    }

    #[test]
    fn rate_limited_error_has_code_and_retry_after() {
        let err: GqlError = GraphQlError::RateLimited {
            retry_after_secs: 30,
        }
        .into();
        assert_eq!(ext_code(&err).as_deref(), Some(CODE_RATE_LIMITED));
        let retry = err
            .extensions
            .as_ref()
            .and_then(|e| e.get("retryAfter"))
            .and_then(|v| match v {
                async_graphql::Value::Number(n) => n.as_u64(),
                _ => None,
            });
        assert_eq!(retry, Some(30));
    }

    #[test]
    fn complexity_error_has_correct_code() {
        let err: GqlError = GraphQlError::Complexity("too deep".into()).into();
        assert_eq!(ext_code(&err).as_deref(), Some(CODE_COMPLEXITY));
    }

    #[test]
    fn database_error_redacts_raw_cause() {
        let err = database_error(&"password=secret");
        assert_eq!(ext_code(&err).as_deref(), Some(CODE_DATABASE));
        // Raw cause must not appear in the client-facing message.
        assert!(!err.message.contains("password"));
        assert!(!err.message.contains("secret"));
    }

    #[test]
    fn internal_error_redacts_raw_cause() {
        let err = internal_error(&"stack trace: src/lib.rs:42");
        assert_eq!(ext_code(&err).as_deref(), Some(CODE_INTERNAL));
        assert!(!err.message.contains("stack trace"));
    }

    #[test]
    fn validation_convenience_fn_includes_field_and_reason() {
        let err = validation_error("amount", "must be positive");
        assert!(err.message.contains("amount"));
        assert!(err.message.contains("must be positive"));
        assert_eq!(ext_code(&err).as_deref(), Some(CODE_VALIDATION));
    }

    #[test]
    fn not_found_convenience_fn() {
        let err = not_found_error("Settlement");
        assert!(err.message.contains("Settlement"));
        assert_eq!(ext_code(&err).as_deref(), Some(CODE_NOT_FOUND));
    }

    #[test]
    fn all_variants_have_non_empty_codes() {
        let errors: Vec<GraphQlError> = vec![
            GraphQlError::Validation("v".into()),
            GraphQlError::NotFound("r".into()),
            GraphQlError::Authentication,
            GraphQlError::Authorization,
            GraphQlError::RateLimited {
                retry_after_secs: 0,
            },
            GraphQlError::Complexity("c".into()),
            GraphQlError::Database("d".into()),
            GraphQlError::Internal("i".into()),
        ];
        for e in errors {
            assert!(!e.code().is_empty());
        }
    }
}
