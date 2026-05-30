//! Input validation for GraphQL operations.
//! Provides secure validation of GraphQL inputs to prevent injection attacks and ensure data integrity.

use regex::Regex;
use std::sync::OnceLock;
use uuid::Uuid;

/// Maximum length for string fields
const MAX_STRING_LENGTH: usize = 255;

/// Maximum length for asset codes
const MAX_ASSET_CODE_LENGTH: usize = 12;

/// Maximum length for Stellar account IDs
const MAX_STELLAR_ACCOUNT_LENGTH: usize = 56;

/// Minimum length for Stellar account IDs
const MIN_STELLAR_ACCOUNT_LENGTH: usize = 56;

/// Cached regex for alphanumeric validation
fn alphanumeric_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"^[a-zA-Z0-9_\-\.]+$").expect("Invalid regex pattern")
    })
}

/// Cached regex for Stellar account validation (public key format)
fn stellar_account_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"^[G][A-Z0-9]{55}$").expect("Invalid regex pattern")
    })
}

/// Validates a string field for length and allowed characters.
///
/// # Arguments
/// * `field_name` - Name of the field being validated (for error messages)
/// * `value` - The string value to validate
/// * `max_length` - Maximum allowed length
///
/// # Returns
/// * `Ok(())` if the string is valid
/// * `Err(String)` with a descriptive error message if invalid
///
/// # Security
/// - Enforces maximum length to prevent DoS attacks
/// - Validates character set to prevent injection attacks
pub fn validate_string_field(field_name: &str, value: &str, max_length: usize) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{} cannot be empty", field_name));
    }

    if value.len() > max_length {
        return Err(format!(
            "{} must not exceed {} characters",
            field_name, max_length
        ));
    }

    if !alphanumeric_pattern().is_match(value) {
        return Err(format!("{} contains invalid characters", field_name));
    }

    Ok(())
}

/// Validates an asset code.
///
/// # Arguments
/// * `asset_code` - The asset code to validate
///
/// # Returns
/// * `Ok(())` if the asset code is valid
/// * `Err(String)` with a descriptive error message if invalid
pub fn validate_asset_code(asset_code: &str) -> Result<(), String> {
    if asset_code.is_empty() {
        return Err("Asset code cannot be empty".to_string());
    }

    if asset_code.len() > MAX_ASSET_CODE_LENGTH {
        return Err(format!(
            "Asset code must not exceed {} characters",
            MAX_ASSET_CODE_LENGTH
        ));
    }

    if !asset_code.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
        return Err("Asset code contains invalid characters".to_string());
    }

    Ok(())
}

/// Validates a Stellar account ID (public key).
///
/// # Arguments
/// * `account` - The Stellar account ID to validate
///
/// # Returns
/// * `Ok(())` if the account ID is valid
/// * `Err(String)` with a descriptive error message if invalid
pub fn validate_stellar_account(account: &str) -> Result<(), String> {
    if account.is_empty() {
        return Err("Stellar account cannot be empty".to_string());
    }

    if account.len() != MIN_STELLAR_ACCOUNT_LENGTH {
        return Err(format!(
            "Stellar account must be exactly {} characters",
            MIN_STELLAR_ACCOUNT_LENGTH
        ));
    }

    if !stellar_account_pattern().is_match(account) {
        return Err("Stellar account has invalid format".to_string());
    }

    Ok(())
}

/// Validates a UUID field.
///
/// # Arguments
/// * `field_name` - Name of the field being validated (for error messages)
/// * `uuid_str` - The UUID string to validate
///
/// # Returns
/// * `Ok(Uuid)` if the UUID is valid
/// * `Err(String)` with a descriptive error message if invalid
pub fn validate_uuid(field_name: &str, uuid_str: &str) -> Result<Uuid, String> {
    match Uuid::parse_str(uuid_str) {
        Ok(uuid) => Ok(uuid),
        Err(_) => Err(format!("{} is not a valid UUID", field_name)),
    }
}

/// Validates pagination limit parameter.
///
/// # Arguments
/// * `limit` - The limit value to validate
///
/// # Returns
/// * `Ok(usize)` with the validated and clamped limit
/// * `Err(String)` with a descriptive error message if invalid
pub fn validate_limit(limit: Option<i64>) -> Result<usize, String> {
    let limit = limit.unwrap_or(20);

    if limit < 1 {
        return Err("Limit must be at least 1".to_string());
    }

    if limit > 100 {
        return Err("Limit must not exceed 100".to_string());
    }

    Ok(limit as usize)
}

/// Validates pagination offset parameter.
///
/// # Arguments
/// * `offset` - The offset value to validate
///
/// # Returns
/// * `Ok(usize)` with the validated offset
/// * `Err(String)` with a descriptive error message if invalid
pub fn validate_offset(offset: Option<i64>) -> Result<usize, String> {
    let offset = offset.unwrap_or(0);

    if offset < 0 {
        return Err("Offset must be non-negative".to_string());
    }

    Ok(offset as usize)
}

/// Sanitizes a string input by removing potentially dangerous characters.
///
/// # Arguments
/// * `input` - The string to sanitize
///
/// # Returns
/// A sanitized string with dangerous characters removed
pub fn sanitize_string(input: &str) -> String {
    input
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_' || *c == '.' || *c == ' ')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_string_field() {
        assert!(validate_string_field("test", "valid_string", 50).is_ok());
    }

    #[test]
    fn test_string_field_too_long() {
        assert!(validate_string_field("test", &"a".repeat(300), 255).is_err());
    }

    #[test]
    fn test_string_field_empty() {
        assert!(validate_string_field("test", "", 50).is_err());
    }

    #[test]
    fn test_string_field_invalid_characters() {
        assert!(validate_string_field("test", "invalid@string", 50).is_err());
    }

    #[test]
    fn test_valid_asset_code() {
        assert!(validate_asset_code("USD").is_ok());
        assert!(validate_asset_code("BTC-TEST").is_ok());
    }

    #[test]
    fn test_asset_code_too_long() {
        assert!(validate_asset_code(&"A".repeat(13)).is_err());
    }

    #[test]
    fn test_asset_code_empty() {
        assert!(validate_asset_code("").is_err());
    }

    #[test]
    fn test_valid_stellar_account() {
        let account = "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWH7";
        assert!(validate_stellar_account(account).is_ok());
    }

    #[test]
    fn test_stellar_account_wrong_length() {
        assert!(validate_stellar_account("GABC").is_err());
    }

    #[test]
    fn test_stellar_account_invalid_format() {
        assert!(validate_stellar_account("XAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").is_err());
    }

    #[test]
    fn test_valid_uuid() {
        let uuid_str = "550e8400-e29b-41d4-a716-446655440000";
        assert!(validate_uuid("id", uuid_str).is_ok());
    }

    #[test]
    fn test_invalid_uuid() {
        assert!(validate_uuid("id", "not-a-uuid").is_err());
    }

    #[test]
    fn test_valid_limit() {
        assert_eq!(validate_limit(Some(50)).unwrap(), 50);
        assert_eq!(validate_limit(None).unwrap(), 20);
    }

    #[test]
    fn test_limit_too_low() {
        assert!(validate_limit(Some(0)).is_err());
    }

    #[test]
    fn test_limit_too_high() {
        assert!(validate_limit(Some(101)).is_err());
    }

    #[test]
    fn test_valid_offset() {
        assert_eq!(validate_offset(Some(10)).unwrap(), 10);
        assert_eq!(validate_offset(None).unwrap(), 0);
    }

    #[test]
    fn test_offset_negative() {
        assert!(validate_offset(Some(-1)).is_err());
    }

    #[test]
    fn test_sanitize_string() {
        let input = "test@string#with$special%chars";
        let sanitized = sanitize_string(input);
        assert_eq!(sanitized, "teststringwithspecialchars");
    }
}
