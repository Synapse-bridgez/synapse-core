//! Input validation for the GraphQL layer.
//!
//! Centralises validation of resolver arguments and filter fields so that
//! schema types and resolver code stay free of ad-hoc string checks.

/// Permitted status values for transaction queries.
const ALLOWED_STATUSES: &[&str] = &[
    "pending",
    "completed",
    "failed",
    "cancelled",
    "processing",
];

/// Maximum length for free-form string filter fields.
const MAX_FILTER_FIELD_LENGTH: usize = 256;

/// Maximum number of rows a single list query may request.
pub const MAX_QUERY_LIMIT: i64 = 1000;

/// Error returned when a GraphQL input field fails validation.
#[derive(Debug, thiserror::Error)]
pub enum InputValidationError {
    #[error("Field '{field}' is too long (max {max} chars)")]
    FieldTooLong { field: &'static str, max: usize },

    #[error("Field '{field}' has an unrecognised value '{value}'")]
    InvalidValue { field: &'static str, value: String },

    #[error("Field '{field}' contains disallowed characters")]
    InvalidCharacters { field: &'static str },

    #[error("Limit {value} exceeds the maximum of {max}")]
    LimitExceeded { value: i64, max: i64 },
}

/// Validates a `status` filter field.
///
/// Only values from [`ALLOWED_STATUSES`] are accepted to prevent injection
/// and to surface typos at the API boundary rather than silently returning
/// empty result sets.
pub fn validate_status(status: &str) -> Result<(), InputValidationError> {
    if status.len() > MAX_FILTER_FIELD_LENGTH {
        return Err(InputValidationError::FieldTooLong {
            field: "status",
            max: MAX_FILTER_FIELD_LENGTH,
        });
    }

    if !ALLOWED_STATUSES.contains(&status) {
        return Err(InputValidationError::InvalidValue {
            field: "status",
            value: status.to_string(),
        });
    }

    Ok(())
}

/// Validates an `asset_code` filter field.
///
/// Asset codes are short alphanumeric identifiers (e.g. `USDC`, `XLM`).
/// Only ASCII alphanumeric characters and hyphens are permitted.
pub fn validate_asset_code(code: &str) -> Result<(), InputValidationError> {
    if code.len() > MAX_FILTER_FIELD_LENGTH {
        return Err(InputValidationError::FieldTooLong {
            field: "asset_code",
            max: MAX_FILTER_FIELD_LENGTH,
        });
    }

    if !code.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(InputValidationError::InvalidCharacters {
            field: "asset_code",
        });
    }

    Ok(())
}

/// Validates a `stellar_account` filter field.
///
/// Stellar account IDs are base32-encoded strings. A lightweight character
/// allowlist is applied here; cryptographic verification of the checksum is
/// delegated to the Stellar SDK at the service layer.
pub fn validate_stellar_account(account: &str) -> Result<(), InputValidationError> {
    if account.len() > MAX_FILTER_FIELD_LENGTH {
        return Err(InputValidationError::FieldTooLong {
            field: "stellar_account",
            max: MAX_FILTER_FIELD_LENGTH,
        });
    }

    if !account.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(InputValidationError::InvalidCharacters {
            field: "stellar_account",
        });
    }

    Ok(())
}

/// Validates a pagination `limit` argument.
///
/// Rejects values outside `[1, MAX_QUERY_LIMIT]` to prevent both empty
/// result requests and unbounded page sizes that could exhaust resources.
pub fn validate_limit(limit: i64) -> Result<(), InputValidationError> {
    if limit < 1 || limit > MAX_QUERY_LIMIT {
        return Err(InputValidationError::LimitExceeded {
            value: limit,
            max: MAX_QUERY_LIMIT,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_statuses() {
        for s in ALLOWED_STATUSES {
            assert!(validate_status(s).is_ok(), "expected {s} to be valid");
        }
    }

    #[test]
    fn test_invalid_status_unknown_value() {
        assert!(validate_status("unknown").is_err());
        assert!(validate_status("PENDING").is_err());
    }

    #[test]
    fn test_invalid_status_injection_attempt() {
        assert!(validate_status("'; DROP TABLE transactions;--").is_err());
    }

    #[test]
    fn test_valid_asset_codes() {
        assert!(validate_asset_code("USDC").is_ok());
        assert!(validate_asset_code("XLM").is_ok());
        assert!(validate_asset_code("USDC-CIRCLE").is_ok());
    }

    #[test]
    fn test_invalid_asset_code_special_chars() {
        assert!(validate_asset_code("USD C").is_err());
        assert!(validate_asset_code("USD@C").is_err());
        assert!(validate_asset_code("USD<script>").is_err());
    }

    #[test]
    fn test_valid_stellar_accounts() {
        assert!(validate_stellar_account("GABC123DEF456").is_ok());
        assert!(validate_stellar_account("GBQVLZE4XCNDFW44GQDYAAPYPFZKLKLXNGKJHYZZTARJQGFN5QKYXW1E").is_ok());
    }

    #[test]
    fn test_invalid_stellar_account_special_chars() {
        assert!(validate_stellar_account("GABC 123").is_err());
        assert!(validate_stellar_account("GABC@123").is_err());
    }

    #[test]
    fn test_valid_limits() {
        assert!(validate_limit(1).is_ok());
        assert!(validate_limit(20).is_ok());
        assert!(validate_limit(MAX_QUERY_LIMIT).is_ok());
    }

    #[test]
    fn test_invalid_limits() {
        assert!(validate_limit(0).is_err());
        assert!(validate_limit(MAX_QUERY_LIMIT + 1).is_err());
        assert!(validate_limit(-1).is_err());
    }

    #[test]
    fn test_field_too_long() {
        let long = "A".repeat(MAX_FILTER_FIELD_LENGTH + 1);
        assert!(validate_asset_code(&long).is_err());
        assert!(validate_stellar_account(&long).is_err());
        assert!(validate_status(&long).is_err());
    }
}
