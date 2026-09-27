//! Centralized error handling for the GraphQL module.
//!
//! # Design
//!
//! GraphQL resolvers must not build `async_graphql::Error` values by hand.
//! Instead every fallible call is converted through a single taxonomy so that
//! clients see:
//!
//! - a stable, machine-readable `extensions.code` on every error, drawn from
//!   the same registry the REST layer uses ([`crate::error::codes`], mirrored
//!   in `docs/error-catalog.md`);
//! - a `message` that has already passed through the shared redaction point
//!   [`crate::error::AppError::client_facing_message`], so raw database,
//!   Redis, or `anyhow` detail can never leak;
//! - `retryAfter` on rate-limit errors (added by
//!   [`crate::graphql::rate_limiting`], which owns the retry hint).
//!
//! `async_graphql` ships a blanket `impl<T: Display> From<T> for Error`, so a
//! bare `?` on a domain error silently produces the lossy, code-less
//! conversion. To keep the taxonomy in force, resolver code converts
//! explicitly with [`GqlResultExt::into_gql`] or
//! [`IntoGraphQlError::into_graphql_error`] rather than `?` / `.into()`.
//!
//! The schema also installs [`crate::graphql::schema`]'s error-taxonomy
//! extension as a backstop: any error that still reaches the response without
//! an `extensions.code` (async-graphql's own parse/validation/complexity
//! errors) is classified there.

use async_graphql::{Error as GqlError, ErrorExtensions};

use crate::error::{codes, AppError};
use crate::graphql::input_validation::InputValidationError;

// === Conversion traits

/// Converts a domain error into an `async_graphql::Error` carrying a stable
/// `extensions.code` and a client-safe message.
pub trait IntoGraphQlError {
    fn into_graphql_error(self) -> GqlError;
}

/// Extension trait for `Result` that maps the error arm through
/// [`IntoGraphQlError`], for use in place of `?` on a raw domain error.
pub trait GqlResultExt<T> {
    fn into_gql(self) -> async_graphql::Result<T>;
}

impl<T, E: IntoGraphQlError> GqlResultExt<T> for Result<T, E> {
    fn into_gql(self) -> async_graphql::Result<T> {
        self.map_err(IntoGraphQlError::into_graphql_error)
    }
}

// === Domain error conversions

impl IntoGraphQlError for AppError {
    fn into_graphql_error(self) -> GqlError {
        // Log the raw cause of wrapping variants before it is redacted away,
        // matching the REST path in `AppError::into_response`.
        self.log_redacted_cause("GraphQL resolver");

        let code = self.code();
        let message = self.client_facing_message();

        GqlError::new(message).extend_with(|_, ext| ext.set("code", code))
    }
}

impl IntoGraphQlError for sqlx::Error {
    fn into_graphql_error(self) -> GqlError {
        // Route through `AppError::from` so `RowNotFound` becomes a 404-class
        // `ERR_NOT_FOUND_001` and every other sqlx error becomes a redacted
        // `ERR_DATABASE_001`, exactly as REST call sites get via `?`.
        AppError::from(self).into_graphql_error()
    }
}

impl IntoGraphQlError for InputValidationError {
    fn into_graphql_error(self) -> GqlError {
        AppError::Validation(self.to_string()).into_graphql_error()
    }
}

// String-returning validators in `crate::graphql::validation` predate the
// typed `InputValidationError`; treat their messages as caller-facing
// validation text.
impl IntoGraphQlError for String {
    fn into_graphql_error(self) -> GqlError {
        AppError::Validation(self).into_graphql_error()
    }
}

// === Convenience constructors

/// Builds a validation error (`ERR_VALIDATION_001`) from a field name and reason.
pub fn validation_error(field: &str, reason: &str) -> GqlError {
    AppError::Validation(format!("Invalid '{field}': {reason}")).into_graphql_error()
}

/// Builds a not-found error (`ERR_NOT_FOUND_001`) for a named resource.
pub fn not_found_error(resource: &str) -> GqlError {
    AppError::NotFound(resource.to_string()).into_graphql_error()
}

/// Builds a database error (`ERR_DATABASE_001`), logging the raw cause
/// server-side. `raw_cause` is never forwarded to the client.
pub fn database_error(raw_cause: &dyn std::fmt::Display) -> GqlError {
    AppError::DatabaseError(raw_cause.to_string()).into_graphql_error()
}

/// Builds an internal error (`ERR_INTERNAL_001`), logging the raw cause
/// server-side. `raw_cause` is never forwarded to the client.
pub fn internal_error(raw_cause: &dyn std::fmt::Display) -> GqlError {
    AppError::Anyhow(anyhow::anyhow!(raw_cause.to_string())).into_graphql_error()
}

// === Backstop for errors that bypass the resolver taxonomy

/// Ensures a `ServerError` carries an `extensions.code`. Called by the schema
/// extension for every error on the outgoing response.
///
/// Errors produced by async-graphql itself (query parsing, field validation,
/// and the depth / complexity / alias limits) never set a code. They are
/// classified here: limit violations map to `ERR_QUERY_COMPLEXITY_001`,
/// everything else to `ERR_BAD_REQUEST_001` (all are client-shaped errors).
pub fn ensure_error_code(err: &mut async_graphql::ServerError) {
    let has_code = err
        .extensions
        .as_ref()
        .map(|e| e.get("code").is_some())
        .unwrap_or(false);
    if has_code {
        return;
    }

    let msg = err.message.to_lowercase();
    let code = if msg.contains("complex")
        || msg.contains("depth")
        || msg.contains("alias")
        || msg.contains("recursion")
        || msg.contains("nested too deep")
    {
        codes::QUERY_COMPLEXITY_001.0
    } else {
        codes::BAD_REQUEST_001.0
    };

    err.extensions
        .get_or_insert_with(Default::default)
        .set("code", code);
}

// === Tests

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

    fn server_ext_code(err: &async_graphql::ServerError) -> Option<String> {
        err.extensions
            .as_ref()
            .and_then(|e| e.get("code"))
            .and_then(|v| match v {
                async_graphql::Value::String(s) => Some(s.clone()),
                _ => None,
            })
    }

    #[test]
    fn validation_error_uses_catalog_code() {
        let err = AppError::Validation("bad input".into()).into_graphql_error();
        assert_eq!(ext_code(&err).as_deref(), Some(codes::VALIDATION_001.0));
        assert!(err.message.contains("bad input"));
    }

    #[test]
    fn not_found_error_uses_catalog_code() {
        let err = not_found_error("Transaction");
        assert_eq!(ext_code(&err).as_deref(), Some(codes::NOT_FOUND_001.0));
    }

    #[test]
    fn sqlx_row_not_found_maps_to_not_found_code() {
        let err = sqlx::Error::RowNotFound.into_graphql_error();
        assert_eq!(ext_code(&err).as_deref(), Some(codes::NOT_FOUND_001.0));
    }

    #[test]
    fn sqlx_error_is_redacted_and_coded() {
        let err = sqlx::Error::ColumnNotFound("internal_secret_column".to_string())
            .into_graphql_error();
        assert_eq!(ext_code(&err).as_deref(), Some(codes::DATABASE_001.0));
        assert!(!err.message.contains("internal_secret_column"));
        assert!(!err.message.contains("does not exist"));
    }

    #[test]
    fn anyhow_internal_error_is_redacted_and_coded() {
        let err = internal_error(&"stack trace: src/lib.rs:42");
        assert_eq!(ext_code(&err).as_deref(), Some(codes::INTERNAL_001.0));
        assert!(!err.message.contains("stack trace"));
    }

    #[test]
    fn input_validation_error_maps_to_validation_code() {
        let err = InputValidationError::LimitExceeded {
            value: 5000,
            max: 1000,
        }
        .into_graphql_error();
        assert_eq!(ext_code(&err).as_deref(), Some(codes::VALIDATION_001.0));
    }

    #[test]
    fn concurrent_modification_maps_to_transaction_006() {
        let err =
            AppError::ConcurrentModification("state changed".into()).into_graphql_error();
        assert_eq!(ext_code(&err).as_deref(), Some(codes::TRANSACTION_006.0));
    }

    #[test]
    fn status_transition_error_maps_to_transaction_005() {
        let err = AppError::InvalidStatusTransition("failed -> completed".into())
            .into_graphql_error();
        assert_eq!(ext_code(&err).as_deref(), Some(codes::TRANSACTION_005.0));
    }

    #[test]
    fn result_ext_maps_error_arm() {
        let r: Result<(), sqlx::Error> = Err(sqlx::Error::RowNotFound);
        let g = r.into_gql().unwrap_err();
        assert_eq!(ext_code(&g).as_deref(), Some(codes::NOT_FOUND_001.0));
    }

    #[test]
    fn ensure_error_code_classifies_complexity() {
        let mut err = async_graphql::ServerError::new(
            "Query is nested too deep: the depth of the query is 12, but the limit is 10",
            None,
        );
        ensure_error_code(&mut err);
        assert_eq!(
            server_ext_code(&err).as_deref(),
            Some(codes::QUERY_COMPLEXITY_001.0)
        );
    }

    #[test]
    fn ensure_error_code_defaults_to_bad_request() {
        let mut err = async_graphql::ServerError::new("Unknown field \"foo\"", None);
        ensure_error_code(&mut err);
        assert_eq!(server_ext_code(&err).as_deref(), Some(codes::BAD_REQUEST_001.0));
    }

    #[test]
    fn ensure_error_code_preserves_existing_code() {
        let mut err = AppError::NotFound("x".into())
            .into_graphql_error()
            .into_server_error(async_graphql::Pos::default());
        ensure_error_code(&mut err);
        assert_eq!(server_ext_code(&err).as_deref(), Some(codes::NOT_FOUND_001.0));
    }
}
