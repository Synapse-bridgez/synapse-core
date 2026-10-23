//! Input validation for telemetry data.
//!
//! Provides secure validation of telemetry inputs to prevent injection attacks
//! and ensure data integrity.

use std::collections::HashMap;

/// Maximum allowed length for string fields
const MAX_STRING_LENGTH: usize = 1024;
/// Maximum allowed number of attributes
const MAX_ATTRIBUTES: usize = 128;
/// Allowed characters in identifiers (alphanumeric, underscore, hyphen, dot)
const IDENTIFIER_PATTERN: &str = r"^[a-zA-Z0-9_\-\.]+$";

/// Validates telemetry input data to prevent injection and format attacks.
///
/// # Health Check
///
/// Performs input validation as a first-line defense in the telemetry health check,
/// preventing malformed or malicious data from reaching the exporter.
#[derive(Debug, Clone)]
pub struct InputValidator;

impl InputValidator {
    /// Validates a span name for correct format.
    ///
    /// # Health Check
    ///
    /// A passing validation means the span name is safe to export (non-empty, correctly sized,
    /// and contains only allowed characters). A failing validation indicates bad input that
    /// should be logged and the span should be dropped.
    ///
    /// # Errors
    /// Returns error if name is empty, too long, or contains invalid characters
    pub fn validate_span_name(name: &str) -> Result<(), ValidationError> {
        if name.is_empty() {
            return Err(ValidationError::EmptyValue(
                "span name cannot be empty".into(),
            ));
        }

        if name.len() > MAX_STRING_LENGTH {
            return Err(ValidationError::TooLong(format!(
                "span name exceeds {} characters",
                MAX_STRING_LENGTH
            )));
        }

        if !regex::Regex::new(IDENTIFIER_PATTERN)
            .unwrap()
            .is_match(name)
        {
            return Err(ValidationError::InvalidFormat(
                "span name contains invalid characters".into(),
            ));
        }

        Ok(())
    }

    /// Validates a string attribute value for safe serialization.
    ///
    /// # Health Check
    ///
    /// A passing validation means the value is safe to include in telemetry exports.
    /// Rejects values that exceed the length limit or contain null bytes (injection vector).
    ///
    /// # Errors
    /// Returns error if value is too long or contains null bytes
    pub fn validate_attribute_value(value: &str) -> Result<(), ValidationError> {
        if value.len() > MAX_STRING_LENGTH {
            return Err(ValidationError::TooLong(format!(
                "attribute value exceeds {} characters",
                MAX_STRING_LENGTH
            )));
        }

        if value.contains('\0') {
            return Err(ValidationError::InvalidFormat(
                "attribute value contains null bytes".into(),
            ));
        }

        Ok(())
    }

    /// Validates a collection of span attributes.
    ///
    /// # Health Check
    ///
    /// A passing validation means all attributes are safe to export and within size bounds.
    /// Rejects if there are too many attributes (prevents DoS) or any attribute is invalid.
    ///
    /// # Errors
    /// Returns error if too many attributes or any attribute is invalid
    pub fn validate_attributes(
        attributes: &HashMap<String, String>,
    ) -> Result<(), ValidationError> {
        if attributes.len() > MAX_ATTRIBUTES {
            return Err(ValidationError::TooMany(format!(
                "too many attributes: {} > {}",
                attributes.len(),
                MAX_ATTRIBUTES
            )));
        }

        for (key, value) in attributes {
            Self::validate_attribute_value(key)?;
            Self::validate_attribute_value(value)?;
        }

        Ok(())
    }

    /// Validates a telemetry exporter endpoint URL.
    ///
    /// # Health Check
    ///
    /// A passing validation means the endpoint is safe to connect to (http/https only,
    /// within length limits). A failing validation indicates SSRF or injection attempt
    /// and should be logged; the connection should not be attempted.
    ///
    /// # Errors
    /// Returns error if endpoint is invalid or potentially malicious
    pub fn validate_endpoint(endpoint: &str) -> Result<(), ValidationError> {
        if endpoint.is_empty() {
            return Err(ValidationError::EmptyValue(
                "endpoint cannot be empty".into(),
            ));
        }

        if !endpoint.starts_with("http://") && !endpoint.starts_with("https://") {
            return Err(ValidationError::InvalidFormat(
                "endpoint must use http or https".into(),
            ));
        }

        if endpoint.len() > 2048 {
            return Err(ValidationError::TooLong("endpoint URL too long".into()));
        }

        Ok(())
    }
}

/// Validation error types
#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("Empty value: {0}")]
    EmptyValue(String),

    #[error("Value too long: {0}")]
    TooLong(String),

    #[error("Invalid format: {0}")]
    InvalidFormat(String),

    #[error("Too many items: {0}")]
    TooMany(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_span_name_valid() {
        assert!(InputValidator::validate_span_name("http.request").is_ok());
        assert!(InputValidator::validate_span_name("db_query").is_ok());
        assert!(InputValidator::validate_span_name("cache-hit").is_ok());
    }

    #[test]
    fn test_validate_span_name_empty() {
        assert!(InputValidator::validate_span_name("").is_err());
    }

    #[test]
    fn test_validate_span_name_too_long() {
        let long_name = "a".repeat(MAX_STRING_LENGTH + 1);
        assert!(InputValidator::validate_span_name(&long_name).is_err());
    }

    #[test]
    fn test_validate_span_name_invalid_chars() {
        assert!(InputValidator::validate_span_name("span@name").is_err());
        assert!(InputValidator::validate_span_name("span name").is_err());
    }

    #[test]
    fn test_validate_attribute_value_valid() {
        assert!(InputValidator::validate_attribute_value("valid_value").is_ok());
        assert!(InputValidator::validate_attribute_value("123").is_ok());
    }

    #[test]
    fn test_validate_attribute_value_null_byte() {
        assert!(InputValidator::validate_attribute_value("value\0null").is_err());
    }

    #[test]
    fn test_validate_attributes_valid() {
        let mut attrs = HashMap::new();
        attrs.insert("key1".to_string(), "value1".to_string());
        attrs.insert("key2".to_string(), "value2".to_string());
        assert!(InputValidator::validate_attributes(&attrs).is_ok());
    }

    #[test]
    fn test_validate_attributes_too_many() {
        let mut attrs = HashMap::new();
        for i in 0..=MAX_ATTRIBUTES {
            attrs.insert(format!("key{}", i), format!("value{}", i));
        }
        assert!(InputValidator::validate_attributes(&attrs).is_err());
    }

    #[test]
    fn test_validate_endpoint_valid() {
        assert!(InputValidator::validate_endpoint("https://localhost:4317").is_ok());
        assert!(InputValidator::validate_endpoint("http://example.com").is_ok());
    }

    #[test]
    fn test_validate_endpoint_invalid_scheme() {
        assert!(InputValidator::validate_endpoint("ftp://example.com").is_err());
        assert!(InputValidator::validate_endpoint("localhost:4317").is_err());
    }

    #[test]
    fn test_validate_endpoint_empty() {
        assert!(InputValidator::validate_endpoint("").is_err());
    }
}
