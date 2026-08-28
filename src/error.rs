//! Application-wide error type and its HTTP mapping.
//!
//! # Client-facing error contract (Part E)
//!
//! - **Safe to expose to clients**: anything derived purely from
//!   caller-controlled input — validation messages, "field X must be
//!   positive", status-transition names, resource identifiers the caller
//!   already supplied. These variants' `Display` text is used directly in
//!   the response body.
//! - **Must be redacted**: any variant that wraps or was built from a raw
//!   external-library error (`sqlx::Error`, `redis::RedisError`,
//!   `anyhow::Error`, or a `String` populated via `some_error.to_string()`)
//!   — these can contain table/column/constraint names, connection detail,
//!   or other internals never meant for a client. `IntoResponse for
//!   AppError` logs the raw cause via `tracing::error!` and substitutes a
//!   generic message before it ever reaches the response body. See
//!   `graphql/error.rs`'s `database_error()`/`internal_error()` for the
//!   same discipline applied on the GraphQL side — this module previously
//!   was not held to it, which is the bug this fixed.
//! - **New variants**: when adding a variant to `AppError`, ask "could this
//!   ever be constructed from `some_lib_error.to_string()`?" If yes, add it
//!   to the redaction match in `IntoResponse::into_response` below rather
//!   than assuming `#[error(...)]`'s `Display` text is automatically safe.
//! - **404 vs 500**: `sqlx::Error::RowNotFound` — "no row matched" — is a
//!   routine, expected condition for a by-id lookup, not a server
//!   malfunction. It is mapped to `AppError::NotFound` (404) centrally in
//!   `From<sqlx::Error> for AppError` below, not left to become a 500 via
//!   the `Database` variant.
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Error codes for programmatic error handling
/// These codes are stable and should never be renamed or reused
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorCode {
    pub code: &'static str,
    pub http_status: u16,
    pub description: &'static str,
}

pub mod codes {
    //! Error code constants
    //! Stable error codes for the API
    //! Format: ERR_<CATEGORY>_<NNN>

    pub const DATABASE_001: (&str, u16, &str) =
        ("ERR_DATABASE_001", 500, "Database connection error");
    pub const DATABASE_002: (&str, u16, &str) =
        ("ERR_DATABASE_002", 500, "Database query execution error");
    pub const VALIDATION_001: (&str, u16, &str) = (
        "ERR_VALIDATION_001",
        400,
        "Validation error - invalid input",
    );
    pub const NOT_FOUND_001: (&str, u16, &str) = ("ERR_NOT_FOUND_001", 404, "Resource not found");
    pub const INTERNAL_001: (&str, u16, &str) = ("ERR_INTERNAL_001", 500, "Internal server error");
    pub const BAD_REQUEST_001: (&str, u16, &str) = (
        "ERR_BAD_REQUEST_001",
        400,
        "Bad request - invalid parameters",
    );
    pub const UNAUTHORIZED_001: (&str, u16, &str) = (
        "ERR_UNAUTHORIZED_001",
        401,
        "Unauthorized - authentication required",
    );

    // Authentication specific errors
    pub const AUTH_001: (&str, u16, &str) =
        ("ERR_AUTH_001", 401, "Invalid authentication credentials");
    pub const AUTH_002: (&str, u16, &str) = ("ERR_AUTH_002", 403, "Insufficient permissions");

    // Transaction specific errors
    pub const TRANSACTION_001: (&str, u16, &str) =
        ("ERR_TRANSACTION_001", 400, "Invalid transaction amount");
    pub const TRANSACTION_002: (&str, u16, &str) = (
        "ERR_TRANSACTION_002",
        400,
        "Transaction amount below minimum",
    );
    pub const TRANSACTION_003: (&str, u16, &str) =
        ("ERR_TRANSACTION_003", 400, "Invalid Stellar address");
    pub const TRANSACTION_004: (&str, u16, &str) = (
        "ERR_TRANSACTION_004",
        409,
        "Transaction already processed (idempotency)",
    );
    pub const TRANSACTION_005: (&str, u16, &str) = (
        "ERR_TRANSACTION_005",
        400,
        "Invalid transaction status transition",
    );

    // Webhook specific errors
    pub const WEBHOOK_001: (&str, u16, &str) =
        ("ERR_WEBHOOK_001", 401, "Invalid webhook signature");
    pub const WEBHOOK_002: (&str, u16, &str) =
        ("ERR_WEBHOOK_002", 400, "Malformed webhook payload");

    pub const TRANSACTION_006: (&str, u16, &str) = (
        "ERR_TRANSACTION_006",
        409,
        "Concurrent modification: transaction state changed during processing",
    );

    // Settlement specific errors
    pub const SETTLEMENT_001: (&str, u16, &str) =
        ("ERR_SETTLEMENT_001", 400, "Invalid settlement amount");
    pub const SETTLEMENT_002: (&str, u16, &str) =
        ("ERR_SETTLEMENT_002", 409, "Settlement already exists");
    pub const SETTLEMENT_003: (&str, u16, &str) = (
        "ERR_SETTLEMENT_003",
        409,
        "Stale transition: settlement state changed during processing",
    );

    // Rate limiting
    pub const RATE_LIMIT_001: (&str, u16, &str) =
        ("ERR_RATE_LIMIT_001", 429, "Rate limit exceeded");

    // Redis errors
    pub const REDIS_001: (&str, u16, &str) = ("ERR_REDIS_001", 500, "Redis operation failed");

    // GraphQL query-shape errors
    pub const QUERY_COMPLEXITY_001: (&str, u16, &str) = (
        "ERR_QUERY_COMPLEXITY_001",
        400,
        "Query exceeds depth, complexity, or alias limits",
    );
}

/// Get all error codes as a vector for catalog generation
pub fn get_all_error_codes() -> Vec<ErrorCode> {
    vec![
        ErrorCode {
            code: codes::DATABASE_001.0,
            http_status: codes::DATABASE_001.1,
            description: codes::DATABASE_001.2,
        },
        ErrorCode {
            code: codes::DATABASE_002.0,
            http_status: codes::DATABASE_002.1,
            description: codes::DATABASE_002.2,
        },
        ErrorCode {
            code: codes::VALIDATION_001.0,
            http_status: codes::VALIDATION_001.1,
            description: codes::VALIDATION_001.2,
        },
        ErrorCode {
            code: codes::NOT_FOUND_001.0,
            http_status: codes::NOT_FOUND_001.1,
            description: codes::NOT_FOUND_001.2,
        },
        ErrorCode {
            code: codes::INTERNAL_001.0,
            http_status: codes::INTERNAL_001.1,
            description: codes::INTERNAL_001.2,
        },
        ErrorCode {
            code: codes::BAD_REQUEST_001.0,
            http_status: codes::BAD_REQUEST_001.1,
            description: codes::BAD_REQUEST_001.2,
        },
        ErrorCode {
            code: codes::UNAUTHORIZED_001.0,
            http_status: codes::UNAUTHORIZED_001.1,
            description: codes::UNAUTHORIZED_001.2,
        },
        ErrorCode {
            code: codes::AUTH_001.0,
            http_status: codes::AUTH_001.1,
            description: codes::AUTH_001.2,
        },
        ErrorCode {
            code: codes::AUTH_002.0,
            http_status: codes::AUTH_002.1,
            description: codes::AUTH_002.2,
        },
        ErrorCode {
            code: codes::TRANSACTION_001.0,
            http_status: codes::TRANSACTION_001.1,
            description: codes::TRANSACTION_001.2,
        },
        ErrorCode {
            code: codes::TRANSACTION_002.0,
            http_status: codes::TRANSACTION_002.1,
            description: codes::TRANSACTION_002.2,
        },
        ErrorCode {
            code: codes::TRANSACTION_003.0,
            http_status: codes::TRANSACTION_003.1,
            description: codes::TRANSACTION_003.2,
        },
        ErrorCode {
            code: codes::TRANSACTION_004.0,
            http_status: codes::TRANSACTION_004.1,
            description: codes::TRANSACTION_004.2,
        },
        ErrorCode {
            code: codes::TRANSACTION_005.0,
            http_status: codes::TRANSACTION_005.1,
            description: codes::TRANSACTION_005.2,
        },
        ErrorCode {
            code: codes::TRANSACTION_006.0,
            http_status: codes::TRANSACTION_006.1,
            description: codes::TRANSACTION_006.2,
        },
        ErrorCode {
            code: codes::WEBHOOK_001.0,
            http_status: codes::WEBHOOK_001.1,
            description: codes::WEBHOOK_001.2,
        },
        ErrorCode {
            code: codes::WEBHOOK_002.0,
            http_status: codes::WEBHOOK_002.1,
            description: codes::WEBHOOK_002.2,
        },
        ErrorCode {
            code: codes::SETTLEMENT_001.0,
            http_status: codes::SETTLEMENT_001.1,
            description: codes::SETTLEMENT_001.2,
        },
        ErrorCode {
            code: codes::SETTLEMENT_002.0,
            http_status: codes::SETTLEMENT_002.1,
            description: codes::SETTLEMENT_002.2,
        },
        ErrorCode {
            code: codes::SETTLEMENT_003.0,
            http_status: codes::SETTLEMENT_003.1,
            description: codes::SETTLEMENT_003.2,
        },
        ErrorCode {
            code: codes::RATE_LIMIT_001.0,
            http_status: codes::RATE_LIMIT_001.1,
            description: codes::RATE_LIMIT_001.2,
        },
        ErrorCode {
            code: codes::REDIS_001.0,
            http_status: codes::REDIS_001.1,
            description: codes::REDIS_001.2,
        },
        ErrorCode {
            code: codes::QUERY_COMPLEXITY_001.0,
            http_status: codes::QUERY_COMPLEXITY_001.1,
            description: codes::QUERY_COMPLEXITY_001.2,
        },
    ]
}

#[derive(Error, Debug)]
pub enum AppError {
    #[error("Database error: {0}")]
    Database(sqlx::Error),

    #[error("Database error: {0}")]
    DatabaseError(String),

    #[error("Validation error: {0}")]
    Validation(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Internal server error: {0}")]
    Internal(String),

    #[error("Bad request: {0}")]
    BadRequest(String),

    #[error("Unauthorized: {0}")]
    Unauthorized(String),

    #[error("Tenant not found")]
    TenantNotFound,

    #[error("Invalid API key or tenant header")]
    InvalidApiKey,

    // Custom errors with specific codes
    #[error("Invalid transaction amount: {0}")]
    InvalidTransactionAmount(String),

    #[error("Amount below minimum: {0}")]
    AmountBelowMinimum(String),

    #[error("Invalid Stellar address: {0}")]
    InvalidStellarAddress(String),

    #[error("Transaction already processed: {0}")]
    TransactionAlreadyProcessed(String),

    #[error("Invalid status transition: {0}")]
    InvalidStatusTransition(String),

    #[error("Concurrent modification: {0}")]
    ConcurrentModification(String),

    #[error("Stale transition: settlement state changed during processing")]
    StaleTransition,

    #[error("Invalid webhook signature")]
    InvalidWebhookSignature,

    #[error("Malformed webhook payload: {0}")]
    MalformedWebhookPayload(String),

    #[error("Invalid settlement amount: {0}")]
    InvalidSettlementAmount(String),

    #[error("Settlement already exists: {0}")]
    SettlementAlreadyExists(String),

    #[error("Rate limit exceeded")]
    RateLimitExceeded,

    #[error("Authentication failed: {0}")]
    AuthenticationFailed(String),

    #[error("Insufficient permissions: {0}")]
    InsufficientPermissions(String),

    #[error("Redis error: {0}")]
    Redis(#[from] redis::RedisError),

    #[error("Internal error: {0}")]
    Anyhow(#[from] anyhow::Error),
}

impl AppError {
    /// Get the HTTP status code for this error
    fn status_code(&self) -> StatusCode {
        match self {
            AppError::Database(_) | AppError::DatabaseError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            AppError::Validation(_) => StatusCode::BAD_REQUEST,
            AppError::NotFound(_) => StatusCode::NOT_FOUND,
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            AppError::TenantNotFound => StatusCode::NOT_FOUND,
            AppError::InvalidApiKey => StatusCode::UNAUTHORIZED,
            AppError::InvalidTransactionAmount(_) => StatusCode::BAD_REQUEST,
            AppError::AmountBelowMinimum(_) => StatusCode::BAD_REQUEST,
            AppError::InvalidStellarAddress(_) => StatusCode::BAD_REQUEST,
            AppError::TransactionAlreadyProcessed(_) => StatusCode::CONFLICT,
            AppError::InvalidStatusTransition(_) => StatusCode::BAD_REQUEST,
            AppError::ConcurrentModification(_) => StatusCode::CONFLICT,
            AppError::StaleTransition => StatusCode::CONFLICT,
            AppError::InvalidWebhookSignature => StatusCode::UNAUTHORIZED,
            AppError::MalformedWebhookPayload(_) => StatusCode::BAD_REQUEST,
            AppError::InvalidSettlementAmount(_) => StatusCode::BAD_REQUEST,
            AppError::SettlementAlreadyExists(_) => StatusCode::CONFLICT,
            AppError::RateLimitExceeded => StatusCode::TOO_MANY_REQUESTS,
            AppError::AuthenticationFailed(_) => StatusCode::UNAUTHORIZED,
            AppError::InsufficientPermissions(_) => StatusCode::FORBIDDEN,
            AppError::Redis(_) => StatusCode::INTERNAL_SERVER_ERROR,
            AppError::Anyhow(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Get the stable error code for this error
    /// These codes are stable and should never be renamed or reused
    pub fn code(&self) -> &'static str {
        match self {
            AppError::Database(_) => codes::DATABASE_001.0,
            AppError::DatabaseError(_) => codes::DATABASE_002.0,
            AppError::Validation(_) => codes::VALIDATION_001.0,
            AppError::NotFound(_) => codes::NOT_FOUND_001.0,
            AppError::Internal(_) => codes::INTERNAL_001.0,
            AppError::BadRequest(_) => codes::BAD_REQUEST_001.0,
            AppError::Unauthorized(_) => codes::UNAUTHORIZED_001.0,
            AppError::TenantNotFound => codes::NOT_FOUND_001.0,
            AppError::InvalidApiKey => codes::UNAUTHORIZED_001.0,
            AppError::InvalidTransactionAmount(_) => codes::TRANSACTION_001.0,
            AppError::AmountBelowMinimum(_) => codes::TRANSACTION_002.0,
            AppError::InvalidStellarAddress(_) => codes::TRANSACTION_003.0,
            AppError::TransactionAlreadyProcessed(_) => codes::TRANSACTION_004.0,
            AppError::InvalidStatusTransition(_) => codes::TRANSACTION_005.0,
            AppError::ConcurrentModification(_) => codes::TRANSACTION_006.0,
            AppError::StaleTransition => codes::SETTLEMENT_003.0,
            AppError::InvalidWebhookSignature => codes::WEBHOOK_001.0,
            AppError::MalformedWebhookPayload(_) => codes::WEBHOOK_002.0,
            AppError::InvalidSettlementAmount(_) => codes::SETTLEMENT_001.0,
            AppError::SettlementAlreadyExists(_) => codes::SETTLEMENT_002.0,
            AppError::RateLimitExceeded => codes::RATE_LIMIT_001.0,
            AppError::AuthenticationFailed(_) => codes::AUTH_001.0,
            AppError::InsufficientPermissions(_) => codes::AUTH_002.0,
            AppError::Redis(_) => codes::REDIS_001.0,
            AppError::Anyhow(_) => codes::INTERNAL_001.0,
        }
    }

    /// The message that is safe to return to a client for this error.
    ///
    /// Variants that wrap or were built from a raw external-library error
    /// (`sqlx`, `redis`, `anyhow`) never forward that error's `Display` text:
    /// it can carry table/column/constraint names or connection detail. Those
    /// variants return a fixed generic string; the caller is responsible for
    /// logging the raw cause first (`IntoResponse for AppError` and the
    /// GraphQL `IntoGraphQlError` impl both do). Every other variant is
    /// derived purely from caller-controlled input and is returned as-is.
    ///
    /// This is the single redaction point shared by the REST (`IntoResponse`)
    /// and GraphQL (`graphql::error`) error paths.
    pub fn client_facing_message(&self) -> String {
        match self {
            AppError::Database(_) | AppError::DatabaseError(_) => {
                "A database error occurred".to_string()
            }
            AppError::Redis(_) => "A backend service error occurred".to_string(),
            AppError::Anyhow(_) => "An internal error occurred".to_string(),
            other => other.to_string(),
        }
    }

    /// Logs the raw cause of a wrapping variant at `error` level so it is
    /// available server-side before [`client_facing_message`] redacts it out
    /// of the client response. A no-op for variants that carry no sensitive
    /// cause.
    ///
    /// [`client_facing_message`]: AppError::client_facing_message
    pub fn log_redacted_cause(&self, context: &str) {
        match self {
            AppError::Database(e) => {
                tracing::error!(context, cause = %e, "redacting raw error cause from client response")
            }
            AppError::DatabaseError(raw) => {
                tracing::error!(context, cause = %raw, "redacting raw error cause from client response")
            }
            AppError::Redis(e) => {
                tracing::error!(context, cause = %e, "redacting raw error cause from client response")
            }
            AppError::Anyhow(e) => {
                tracing::error!(context, cause = %e, "redacting raw error cause from client response")
            }
            _ => {}
        }
    }
}

/// Part E fix: `?` on a `sqlx::Error` used to always become
/// `AppError::Database`, which maps to a 500 — including for
/// `sqlx::Error::RowNotFound`, an extremely common, entirely routine "no
/// row matched this lookup" condition that should be a 404, not a message
/// implying the server malfunctioned. This is a hand-written `From` (not
/// `#[from]` on the `Database` variant) specifically so every existing call
/// site that already does `sqlx_call().await?` gets the fix automatically,
/// without auditing and touching each one individually.
impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        match e {
            sqlx::Error::RowNotFound => AppError::NotFound("Resource not found".to_string()),
            other => AppError::Database(other),
        }
    }
}

/// Extension type to carry request ID through the request lifecycle.
#[derive(Clone, Debug)]
pub struct RequestId(pub String);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let timestamp = chrono::Utc::now().to_rfc3339();
        let code = self.code();
        let docs_url = format!("/errors#{code}");

        // Part E fix: variants that wrap or were built from a raw external
        // error (sqlx, redis, anyhow) must never forward that error's
        // `Display` text to the client — it can include table/column names,
        // constraint names, connection strings, or other internal detail.
        // The redaction itself now lives in `AppError::client_facing_message`
        // so the REST and GraphQL error paths share one implementation; here
        // we only log the raw cause before it is dropped.
        self.log_redacted_cause("AppError::into_response");
        let error_message = self.client_facing_message();

        // Generate actionable detail message
        let detail = match &self {
            AppError::InvalidTransactionAmount(msg) => {
                format!("Amount must be a positive number. {msg}")
            }
            AppError::AmountBelowMinimum(msg) => {
                format!("Amount is below the minimum threshold. {msg}")
            }
            AppError::InvalidStellarAddress(msg) => {
                format!("Stellar address must be 56 characters starting with 'G'. {msg}")
            }
            AppError::InvalidStatusTransition(msg) => {
                format!("Status transition is not allowed. {msg}")
            }
            AppError::Validation(msg) => {
                format!("Validation failed. {msg}")
            }
            _ => error_message.clone(),
        };

        let body = serde_json::json!({
            "error": error_message,
            "code": code,
            "status": status.as_u16(),
            "timestamp": timestamp,
            "detail": detail,
            "docs_url": docs_url,
        });

        (status, Json(body)).into_response()
    }
}

/// Error response structure for JSON serialization
#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
    pub code: String,
    pub status: u16,
}

/// Catalog response structure
#[derive(Debug, Serialize, Deserialize)]
#[serde(bound(deserialize = "'de: 'static"))]
pub struct ErrorCatalogResponse {
    pub errors: Vec<ErrorCode>,
    pub version: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validation_error_status_code() {
        let error = AppError::Validation("Invalid input".to_string());
        assert_eq!(error.status_code(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn test_not_found_error_status_code() {
        let error = AppError::NotFound("Resource not found".to_string());
        assert_eq!(error.status_code(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn test_database_error_status_code() {
        let error = AppError::Database(sqlx::Error::RowNotFound);
        assert_eq!(error.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn test_internal_error_status_code() {
        let error = AppError::Internal("Something went wrong".to_string());
        assert_eq!(error.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn test_bad_request_error_status_code() {
        let error = AppError::BadRequest("Bad request".to_string());
        assert_eq!(error.status_code(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn test_unauthorized_error_status_code() {
        let error = AppError::Unauthorized("Unauthorized access".to_string());
        assert_eq!(error.status_code(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_validation_error_response() {
        let error = AppError::Validation("Invalid email format".to_string());
        let response = error.into_response();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_not_found_error_response() {
        let error = AppError::NotFound("User not found".to_string());
        let response = error.into_response();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_database_error_response() {
        let error = AppError::Database(sqlx::Error::RowNotFound);
        let response = error.into_response();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn test_error_codes() {
        // Test that all error types return correct codes
        assert_eq!(
            AppError::Validation("test".to_string()).code(),
            codes::VALIDATION_001.0
        );
        assert_eq!(
            AppError::NotFound("test".to_string()).code(),
            codes::NOT_FOUND_001.0
        );
        assert_eq!(
            AppError::BadRequest("test".to_string()).code(),
            codes::BAD_REQUEST_001.0
        );
        assert_eq!(
            AppError::Unauthorized("test".to_string()).code(),
            codes::UNAUTHORIZED_001.0
        );
        assert_eq!(
            AppError::Internal("test".to_string()).code(),
            codes::INTERNAL_001.0
        );
        assert_eq!(
            AppError::Database(sqlx::Error::RowNotFound).code(),
            codes::DATABASE_001.0
        );
        assert_eq!(
            AppError::DatabaseError("test".to_string()).code(),
            codes::DATABASE_002.0
        );

        // Custom errors
        assert_eq!(
            AppError::InvalidTransactionAmount("test".to_string()).code(),
            codes::TRANSACTION_001.0
        );
        assert_eq!(
            AppError::AmountBelowMinimum("test".to_string()).code(),
            codes::TRANSACTION_002.0
        );
        assert_eq!(
            AppError::InvalidStellarAddress("test".to_string()).code(),
            codes::TRANSACTION_003.0
        );
        assert_eq!(
            AppError::TransactionAlreadyProcessed("test".to_string()).code(),
            codes::TRANSACTION_004.0
        );
        assert_eq!(
            AppError::InvalidStatusTransition("test".to_string()).code(),
            codes::TRANSACTION_005.0
        );
        assert_eq!(
            AppError::InvalidWebhookSignature.code(),
            codes::WEBHOOK_001.0
        );
        assert_eq!(
            AppError::MalformedWebhookPayload("test".to_string()).code(),
            codes::WEBHOOK_002.0
        );
        assert_eq!(
            AppError::InvalidSettlementAmount("test".to_string()).code(),
            codes::SETTLEMENT_001.0
        );
        assert_eq!(
            AppError::SettlementAlreadyExists("test".to_string()).code(),
            codes::SETTLEMENT_002.0
        );
        assert_eq!(
            AppError::ConcurrentModification("test".to_string()).code(),
            codes::TRANSACTION_006.0
        );
        assert_eq!(AppError::RateLimitExceeded.code(), codes::RATE_LIMIT_001.0);
        assert_eq!(
            AppError::AuthenticationFailed("test".to_string()).code(),
            codes::AUTH_001.0
        );
        assert_eq!(
            AppError::InsufficientPermissions("test".to_string()).code(),
            codes::AUTH_002.0
        );
    }

    /// Part E regression test: no `Database`-variant response body may
    /// contain the raw sqlx error text (which can include column/constraint
    /// names or other internal detail).
    #[tokio::test]
    async fn test_database_error_response_redacts_raw_sql_detail() {
        let raw_detail = "column \"internal_secret_column\" does not exist";
        let error = AppError::Database(sqlx::Error::ColumnNotFound(raw_detail.to_string()));
        let response = error.into_response();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let bytes = hyper::body::to_bytes(response.into_body()).await.unwrap();
        let body_str = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(
            !body_str.contains("internal_secret_column"),
            "response body leaked raw column name: {body_str}"
        );
        assert!(
            !body_str.contains("does not exist"),
            "response body leaked raw sqlx error text: {body_str}"
        );
    }

    /// Same guarantee for the `DatabaseError(String)` variant, which several
    /// call sites populate directly from `sqlx::Error::to_string()`.
    #[tokio::test]
    async fn test_database_error_string_variant_redacts_raw_detail() {
        let error = AppError::DatabaseError(
            "duplicate key value violates unique constraint \"transactions_pkey\"".to_string(),
        );
        let response = error.into_response();

        let bytes = hyper::body::to_bytes(response.into_body()).await.unwrap();
        let body_str = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(
            !body_str.contains("transactions_pkey"),
            "response body leaked raw constraint name: {body_str}"
        );
    }

    /// Part E regression test: a lookup that finds no row must map to a
    /// routine 404, not a 500 implying the server malfunctioned. This
    /// exercises the exact path every `sqlx_call().await?` site goes
    /// through (`From<sqlx::Error> for AppError`), not just a manually
    /// constructed variant.
    #[test]
    fn test_row_not_found_maps_to_404_via_from_conversion() {
        let error: AppError = sqlx::Error::RowNotFound.into();
        assert!(matches!(error, AppError::NotFound(_)));
        assert_eq!(error.status_code(), StatusCode::NOT_FOUND);
    }

    /// Non-`RowNotFound` sqlx errors must still map to the redacted
    /// `Database` variant (500), not silently become a 404.
    #[test]
    fn test_other_sqlx_errors_still_map_to_database_variant() {
        let error: AppError = sqlx::Error::PoolClosed.into();
        assert!(matches!(error, AppError::Database(_)));
        assert_eq!(error.status_code(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn test_error_catalog_size() {
        let catalog = get_all_error_codes();
        // Verify we have all expected error codes
        assert!(
            catalog.len() >= 19,
            "Error catalog should have at least 19 codes"
        );
    }

    #[test]
    fn test_concurrent_modification_maps_to_conflict() {
        let error = AppError::ConcurrentModification("state changed".to_string());
        assert_eq!(error.status_code(), StatusCode::CONFLICT);
        assert_eq!(error.code(), codes::TRANSACTION_006.0);
    }

    /// Every code returned by `get_all_error_codes` must be documented in
    /// `docs/error-catalog.md`, and every `ERR_*` code appearing in that doc
    /// must be a real code. This keeps the catalog, the REST layer, and the
    /// GraphQL layer (which reuses these codes in `extensions.code`) in sync.
    #[test]
    fn test_catalog_doc_matches_code_registry() {
        let doc = include_str!("../docs/error-catalog.md");
        let registry: std::collections::HashSet<&str> =
            get_all_error_codes().into_iter().map(|c| c.code).collect();

        for code in &registry {
            assert!(
                doc.contains(*code),
                "code {code} is in get_all_error_codes() but missing from docs/error-catalog.md"
            );
        }

        let code_pattern = regex::Regex::new(r"ERR_[A-Z0-9_]*[0-9]{3}").unwrap();
        for m in code_pattern.find_iter(doc) {
            let token = m.as_str();
            assert!(
                registry.contains(token),
                "docs/error-catalog.md documents {token}, which is not in get_all_error_codes()"
            );
        }
    }

    #[tokio::test]
    async fn test_concurrent_modification_response_is_not_redacted() {
        let error = AppError::ConcurrentModification(
            "transaction was completed by a concurrent request".to_string(),
        );
        let response = error.into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);

        let bytes = hyper::body::to_bytes(response.into_body()).await.unwrap();
        let body_str = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(body_str.contains("concurrent request"));
        assert!(body_str.contains(codes::TRANSACTION_006.0));
    }
}
