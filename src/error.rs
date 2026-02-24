use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
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

    // Settlement specific errors
    pub const SETTLEMENT_001: (&str, u16, &str) =
        ("ERR_SETTLEMENT_001", 400, "Invalid settlement amount");
    pub const SETTLEMENT_002: (&str, u16, &str) =
        ("ERR_SETTLEMENT_002", 409, "Settlement already exists");

    // Rate limiting
    pub const RATE_LIMIT_001: (&str, u16, &str) =
        ("ERR_RATE_LIMIT_001", 429, "Rate limit exceeded");
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
            code: codes::RATE_LIMIT_001.0,
            http_status: codes::RATE_LIMIT_001.1,
            description: codes::RATE_LIMIT_001.2,
        },
    ]
}

#[derive(Error, Debug)]
pub enum AppError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

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
            AppError::InvalidTransactionAmount(_) => StatusCode::BAD_REQUEST,
            AppError::AmountBelowMinimum(_) => StatusCode::BAD_REQUEST,
            AppError::InvalidStellarAddress(_) => StatusCode::BAD_REQUEST,
            AppError::TransactionAlreadyProcessed(_) => StatusCode::CONFLICT,
            AppError::InvalidStatusTransition(_) => StatusCode::BAD_REQUEST,
            AppError::InvalidWebhookSignature => StatusCode::UNAUTHORIZED,
            AppError::MalformedWebhookPayload(_) => StatusCode::BAD_REQUEST,
            AppError::InvalidSettlementAmount(_) => StatusCode::BAD_REQUEST,
            AppError::SettlementAlreadyExists(_) => StatusCode::CONFLICT,
            AppError::RateLimitExceeded => StatusCode::TOO_MANY_REQUESTS,
            AppError::AuthenticationFailed(_) => StatusCode::UNAUTHORIZED,
            AppError::InsufficientPermissions(_) => StatusCode::FORBIDDEN,
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
            AppError::InvalidTransactionAmount(_) => codes::TRANSACTION_001.0,
            AppError::AmountBelowMinimum(_) => codes::TRANSACTION_002.0,
            AppError::InvalidStellarAddress(_) => codes::TRANSACTION_003.0,
            AppError::TransactionAlreadyProcessed(_) => codes::TRANSACTION_004.0,
            AppError::InvalidStatusTransition(_) => codes::TRANSACTION_005.0,
            AppError::InvalidWebhookSignature => codes::WEBHOOK_001.0,
            AppError::MalformedWebhookPayload(_) => codes::WEBHOOK_002.0,
            AppError::InvalidSettlementAmount(_) => codes::SETTLEMENT_001.0,
            AppError::SettlementAlreadyExists(_) => codes::SETTLEMENT_002.0,
            AppError::RateLimitExceeded => codes::RATE_LIMIT_001.0,
            AppError::AuthenticationFailed(_) => codes::AUTH_001.0,
            AppError::InsufficientPermissions(_) => codes::AUTH_002.0,
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let body = Json(json!({
            "error": self.to_string(),
            "code": self.code(),
            "status": status.as_u16(),
        }));

        (status, body).into_response()
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

    #[test]
    fn test_error_catalog_size() {
        let catalog = get_all_error_codes();
        // Verify we have all expected error codes
        assert!(
            catalog.len() >= 19,
            "Error catalog should have at least 19 codes"
        );
    }
}
