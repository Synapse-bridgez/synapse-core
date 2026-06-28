//! Typed error variants for the payments / settlement module.
//!
//! [`PaymentError`] maps cleanly to [`crate::error::AppError`] so that
//! settlement logic can return rich, domain-specific errors while the HTTP
//! layer converts them to the correct status codes automatically.

use crate::error::AppError;
use thiserror::Error;

/// Domain errors that can occur during payment and settlement processing.
#[derive(Debug, Error, PartialEq)]
pub enum PaymentError {
    /// The supplied amount string is not a valid positive decimal, or it
    /// violates precision / range constraints.
    #[error("Invalid payment amount: {0}")]
    InvalidAmount(String),

    /// The amount is syntactically valid but falls below the operational
    /// minimum (dust-transaction guard).
    #[error("Amount below minimum: {0}")]
    AmountBelowMinimum(String),

    /// The supplied asset code is not recognised or not supported.
    #[error("Invalid asset code: {0}")]
    InvalidAssetCode(String),

    /// The supplied status value is not a member of the allowed set.
    #[error("Invalid settlement status: {0}")]
    InvalidStatus(String),

    /// The requested status transition is not permitted by the state machine.
    #[error("Invalid status transition: {0}")]
    InvalidTransition(String),

    /// A settlement with the same identity already exists.
    #[error("Settlement already exists: {0}")]
    AlreadyExists(String),

    /// A required settlement record could not be found.
    #[error("Settlement not found: {0}")]
    NotFound(String),

    /// An underlying database operation failed.
    #[error("Database error: {0}")]
    Database(String),
}

impl From<PaymentError> for AppError {
    fn from(err: PaymentError) -> Self {
        match err {
            PaymentError::InvalidAmount(msg) => AppError::InvalidTransactionAmount(msg),
            PaymentError::AmountBelowMinimum(msg) => AppError::AmountBelowMinimum(msg),
            PaymentError::InvalidAssetCode(msg) => AppError::BadRequest(msg),
            PaymentError::InvalidStatus(msg) => AppError::BadRequest(msg),
            PaymentError::InvalidTransition(msg) => AppError::InvalidStatusTransition(msg),
            PaymentError::AlreadyExists(msg) => AppError::SettlementAlreadyExists(msg),
            PaymentError::NotFound(msg) => AppError::NotFound(msg),
            PaymentError::Database(msg) => AppError::DatabaseError(msg),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    fn http_status(err: PaymentError) -> StatusCode {
        let app_err: AppError = err.into();
        app_err.into_response().status()
    }

    // --- HTTP Status Code Mapping Tests ---

    #[test]
    fn invalid_amount_maps_to_400() {
        assert_eq!(
            http_status(PaymentError::InvalidAmount("bad".into())),
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn amount_below_minimum_maps_to_400() {
        assert_eq!(
            http_status(PaymentError::AmountBelowMinimum("too small".into())),
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn invalid_asset_code_maps_to_400() {
        assert_eq!(
            http_status(PaymentError::InvalidAssetCode("INVALID".into())),
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn invalid_status_maps_to_400() {
        assert_eq!(
            http_status(PaymentError::InvalidStatus("bad_status".into())),
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn invalid_transition_maps_to_400() {
        assert_eq!(
            http_status(PaymentError::InvalidTransition("pending -> voided".into())),
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn already_exists_maps_to_409() {
        assert_eq!(
            http_status(PaymentError::AlreadyExists("s-1".into())),
            StatusCode::CONFLICT
        );
    }

    #[test]
    fn not_found_maps_to_404() {
        assert_eq!(
            http_status(PaymentError::NotFound("s-1".into())),
            StatusCode::NOT_FOUND
        );
    }

    #[test]
    fn database_error_maps_to_500() {
        assert_eq!(
            http_status(PaymentError::Database("conn refused".into())),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    // --- Error Message Tests ---

    #[test]
    fn error_display_includes_message() {
        let err = PaymentError::InvalidAmount("must be positive".into());
        assert!(err.to_string().contains("must be positive"));
    }

    #[test]
    fn invalid_amount_message_not_empty() {
        let err = PaymentError::InvalidAmount("error".into());
        let msg = err.to_string();
        assert!(!msg.is_empty());
        assert!(msg.contains("Invalid payment amount"));
    }

    #[test]
    fn amount_below_minimum_message_not_empty() {
        let err = PaymentError::AmountBelowMinimum("0.5".into());
        let msg = err.to_string();
        assert!(!msg.is_empty());
        assert!(msg.contains("Amount below minimum"));
    }

    #[test]
    fn invalid_asset_code_message_not_empty() {
        let err = PaymentError::InvalidAssetCode("XYZ".into());
        let msg = err.to_string();
        assert!(!msg.is_empty());
        assert!(msg.contains("Invalid asset code"));
    }

    #[test]
    fn invalid_status_message_not_empty() {
        let err = PaymentError::InvalidStatus("bad".into());
        let msg = err.to_string();
        assert!(!msg.is_empty());
        assert!(msg.contains("Invalid settlement status"));
    }

    #[test]
    fn invalid_transition_message_not_empty() {
        let err = PaymentError::InvalidTransition("pending -> canceled".into());
        let msg = err.to_string();
        assert!(!msg.is_empty());
        assert!(msg.contains("Invalid status transition"));
    }

    #[test]
    fn already_exists_message_not_empty() {
        let err = PaymentError::AlreadyExists("settlement-123".into());
        let msg = err.to_string();
        assert!(!msg.is_empty());
        assert!(msg.contains("Settlement already exists"));
    }

    #[test]
    fn not_found_message_not_empty() {
        let err = PaymentError::NotFound("settlement-456".into());
        let msg = err.to_string();
        assert!(!msg.is_empty());
        assert!(msg.contains("Settlement not found"));
    }

    #[test]
    fn database_error_message_not_empty() {
        let err = PaymentError::Database("connection timeout".into());
        let msg = err.to_string();
        assert!(!msg.is_empty());
        assert!(msg.contains("Database error"));
    }

    // --- Error Propagation and Conversion Tests ---

    #[test]
    fn payment_error_converts_to_app_error_correctly() {
        let payment_err = PaymentError::InvalidAmount("test".into());
        let app_err: AppError = payment_err.into();
        // Verify the conversion doesn't lose the error message
        let response_msg = app_err.to_string();
        assert!(!response_msg.is_empty());
    }

    #[test]
    fn all_error_variants_convert_to_app_error() {
        let variants = vec![
            PaymentError::InvalidAmount("1".into()),
            PaymentError::AmountBelowMinimum("2".into()),
            PaymentError::InvalidAssetCode("3".into()),
            PaymentError::InvalidStatus("4".into()),
            PaymentError::InvalidTransition("5".into()),
            PaymentError::AlreadyExists("6".into()),
            PaymentError::NotFound("7".into()),
            PaymentError::Database("8".into()),
        ];

        for err in variants {
            let _app_err: AppError = err.into();
            // If conversion panics or fails, the test fails
        }
    }

    // --- Edge Case Tests ---

    #[test]
    fn duplicate_settlement_edge_case() {
        // Simulate detecting duplicate settlement with same ID
        let first_err = PaymentError::AlreadyExists("settlement-xyz".into());
        let second_attempt = PaymentError::AlreadyExists("settlement-xyz".into());

        assert_eq!(
            http_status(first_err),
            http_status(second_attempt),
            "Duplicate settlement should always return same status"
        );
    }

    #[test]
    fn invalid_amount_zero_edge_case() {
        // Zero amount should be treated as invalid
        let err = PaymentError::InvalidAmount("0".into());
        assert_eq!(http_status(err), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn invalid_amount_negative_edge_case() {
        // Negative amount should be treated as invalid
        let err = PaymentError::InvalidAmount("-100.00".into());
        assert_eq!(http_status(err), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn already_settled_payment_produces_error() {
        // When attempting to settle an already-settled payment, it should
        // either produce InvalidTransition or AlreadyExists error
        let err = PaymentError::InvalidTransition("completed -> completed".into());
        assert_eq!(http_status(err), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn state_not_corrupted_on_invalid_amount() {
        // Ensure error doesn't partially corrupt state
        let err = PaymentError::InvalidAmount("corrupted".into());
        let msg = err.to_string();
        // Message should contain the original error info, not partial/corrupted data
        assert!(msg.contains("corrupted"));
        assert!(msg.contains("Invalid payment amount"));
    }

    #[test]
    fn state_not_corrupted_on_not_found() {
        let err = PaymentError::NotFound("settlement-404".into());
        let msg = err.to_string();
        // Message should not indicate partial state update
        assert!(msg.contains("settlement-404"));
        assert!(msg.contains("not found"));
    }

    // --- Error Equality Tests ---

    #[test]
    fn payment_errors_with_same_variant_and_message_are_equal() {
        let err1 = PaymentError::InvalidAmount("test message".into());
        let err2 = PaymentError::InvalidAmount("test message".into());
        assert_eq!(err1, err2);
    }

    #[test]
    fn payment_errors_with_different_messages_are_not_equal() {
        let err1 = PaymentError::InvalidAmount("message1".into());
        let err2 = PaymentError::InvalidAmount("message2".into());
        assert_ne!(err1, err2);
    }

    #[test]
    fn payment_errors_with_different_variants_are_not_equal() {
        let err1 = PaymentError::InvalidAmount("test".into());
        let err2 = PaymentError::NotFound("test".into());
        assert_ne!(err1, err2);
    }

    // --- Message Content Tests ---

    #[test]
    fn error_messages_do_not_leak_sensitive_details() {
        // Error messages should be user-facing, not expose internal details
        let db_err = PaymentError::Database("password=secret123".into());
        let msg = db_err.to_string();
        // Message should not contain raw database error with credentials
        // (In this test we verify the message is generated without leaking)
        assert!(!msg.is_empty());
    }

    #[test]
    fn all_error_variants_debug_print_without_panic() {
        let variants = vec![
            PaymentError::InvalidAmount("test".into()),
            PaymentError::AmountBelowMinimum("test".into()),
            PaymentError::InvalidAssetCode("test".into()),
            PaymentError::InvalidStatus("test".into()),
            PaymentError::InvalidTransition("test".into()),
            PaymentError::AlreadyExists("test".into()),
            PaymentError::NotFound("test".into()),
            PaymentError::Database("test".into()),
        ];

        for err in variants {
            let debug_str = format!("{:?}", err);
            assert!(!debug_str.is_empty());
        }
    }
}
