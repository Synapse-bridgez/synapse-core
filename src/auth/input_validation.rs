//! Input validation for authentication operations.
//! Provides secure validation of API keys, tokens, and credentials.

use regex::Regex;
use std::sync::OnceLock;

/// Minimum API key length
const MIN_API_KEY_LENGTH: usize = 32;

/// Maximum API key length
const MAX_API_KEY_LENGTH: usize = 256;

/// Minimum token length
const MIN_TOKEN_LENGTH: usize = 20;

/// Maximum token length
const MAX_TOKEN_LENGTH: usize = 1024;

/// Cached regex for API key validation
fn api_key_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"^[a-zA-Z0-9_\-\.]+$").expect("Invalid regex pattern"))
}

/// Validates an API key for format and length.
///
/// # Arguments
/// * `key` - The API key to validate
///
/// # Returns
/// * `Ok(())` if the key is valid
/// * `Err(String)` with a descriptive error message if invalid
///
/// # Security
/// - Rejects keys outside the allowed length range
/// - Rejects keys with invalid characters
/// - Prevents injection attacks through character restrictions
pub fn validate_api_key(key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err("API key cannot be empty".to_string());
    }

    if key.len() < MIN_API_KEY_LENGTH {
        return Err(format!(
            "API key must be at least {} characters long",
            MIN_API_KEY_LENGTH
        ));
    }

    if key.len() > MAX_API_KEY_LENGTH {
        return Err(format!(
            "API key must not exceed {} characters",
            MAX_API_KEY_LENGTH
        ));
    }

    if !api_key_pattern().is_match(key) {
        return Err("API key contains invalid characters".to_string());
    }

    Ok(())
}

/// Validates a bearer token for format and length.
///
/// # Arguments
/// * `token` - The token to validate
///
/// # Returns
/// * `Ok(())` if the token is valid
/// * `Err(String)` with a descriptive error message if invalid
pub fn validate_token(token: &str) -> Result<(), String> {
    if token.is_empty() {
        return Err("Token cannot be empty".to_string());
    }

    if token.len() < MIN_TOKEN_LENGTH {
        return Err(format!(
            "Token must be at least {} characters long",
            MIN_TOKEN_LENGTH
        ));
    }

    if token.len() > MAX_TOKEN_LENGTH {
        return Err(format!(
            "Token must not exceed {} characters",
            MAX_TOKEN_LENGTH
        ));
    }

    Ok(())
}

/// Validates an authorization header value.
///
/// # Arguments
/// * `header` - The Authorization header value (e.g., "Bearer token123")
///
/// # Returns
/// * `Ok(token)` with the extracted token if valid
/// * `Err(String)` with a descriptive error message if invalid
pub fn validate_auth_header(header: &str) -> Result<String, String> {
    let parts: Vec<&str> = header.split_whitespace().collect();

    if parts.len() != 2 {
        return Err("Authorization header must contain exactly 2 parts".to_string());
    }

    if parts[0].to_lowercase() != "bearer" {
        return Err("Authorization header must use Bearer scheme".to_string());
    }

    let token = parts[1];
    validate_token(token)?;
    Ok(token.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_api_key() {
        let key = "a".repeat(MIN_API_KEY_LENGTH);
        assert!(validate_api_key(&key).is_ok());
    }

    #[test]
    fn test_api_key_too_short() {
        let key = "a".repeat(MIN_API_KEY_LENGTH - 1);
        assert!(validate_api_key(&key).is_err());
    }

    #[test]
    fn test_api_key_too_long() {
        let key = "a".repeat(MAX_API_KEY_LENGTH + 1);
        assert!(validate_api_key(&key).is_err());
    }

    #[test]
    fn test_api_key_empty() {
        assert!(validate_api_key("").is_err());
    }

    #[test]
    fn test_api_key_invalid_characters() {
        let key = "a".repeat(MIN_API_KEY_LENGTH);
        let invalid = format!("{}@", key);
        assert!(validate_api_key(&invalid).is_err());
    }

    #[test]
    fn test_valid_token() {
        let token = "a".repeat(MIN_TOKEN_LENGTH);
        assert!(validate_token(&token).is_ok());
    }

    #[test]
    fn test_token_too_short() {
        let token = "a".repeat(MIN_TOKEN_LENGTH - 1);
        assert!(validate_token(&token).is_err());
    }

    #[test]
    fn test_token_too_long() {
        let token = "a".repeat(MAX_TOKEN_LENGTH + 1);
        assert!(validate_token(&token).is_err());
    }

    #[test]
    fn test_valid_auth_header() {
        let token = "a".repeat(MIN_TOKEN_LENGTH);
        let header = format!("Bearer {}", token);
        let result = validate_auth_header(&header);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), token);
    }

    #[test]
    fn test_auth_header_invalid_scheme() {
        let token = "a".repeat(MIN_TOKEN_LENGTH);
        let header = format!("Basic {}", token);
        assert!(validate_auth_header(&header).is_err());
    }

    #[test]
    fn test_auth_header_missing_token() {
        assert!(validate_auth_header("Bearer").is_err());
    }

    #[test]
    fn test_auth_header_case_insensitive_scheme() {
        let token = "a".repeat(MIN_TOKEN_LENGTH);
        let header = format!("bearer {}", token);
        let result = validate_auth_header(&header);
        assert!(result.is_ok());
    }
}
