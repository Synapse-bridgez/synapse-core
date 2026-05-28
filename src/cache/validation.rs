/// Input validation for cache operations

#[derive(Debug, Clone)]
pub struct CacheValidator;

#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("Invalid key: {0}")]
    InvalidKey(String),
    #[error("Invalid value: {0}")]
    InvalidValue(String),
    #[error("Key too long: max 512 bytes")]
    KeyTooLong,
    #[error("Value too large: max 512MB")]
    ValueTooLarge,
    #[error("Invalid TTL: must be positive")]
    InvalidTTL,
}

impl CacheValidator {
    /// Validate cache key format and length
    pub fn validate_key(key: &str) -> Result<(), ValidationError> {
        if key.is_empty() {
            return Err(ValidationError::InvalidKey("key cannot be empty".to_string()));
        }

        if key.len() > 512 {
            return Err(ValidationError::KeyTooLong);
        }

        if !key.chars().all(|c| c.is_alphanumeric() || c == '_' || c == ':' || c == '-') {
            return Err(ValidationError::InvalidKey(
                "key contains invalid characters".to_string(),
            ));
        }

        Ok(())
    }

    /// Validate cache value size
    pub fn validate_value_size(value: &[u8]) -> Result<(), ValidationError> {
        const MAX_SIZE: usize = 512 * 1024 * 1024; // 512MB
        if value.len() > MAX_SIZE {
            return Err(ValidationError::ValueTooLarge);
        }
        Ok(())
    }

    /// Validate TTL (time to live) in seconds
    pub fn validate_ttl(ttl: i64) -> Result<(), ValidationError> {
        if ttl <= 0 {
            return Err(ValidationError::InvalidTTL);
        }
        Ok(())
    }

    /// Validate key-value pair for cache storage
    pub fn validate_entry(key: &str, value: &[u8], ttl: Option<i64>) -> Result<(), ValidationError> {
        Self::validate_key(key)?;
        Self::validate_value_size(value)?;
        if let Some(ttl_val) = ttl {
            Self::validate_ttl(ttl_val)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_key_valid() {
        assert!(CacheValidator::validate_key("cache:user:123").is_ok());
        assert!(CacheValidator::validate_key("session_abc").is_ok());
        assert!(CacheValidator::validate_key("key-with-dash").is_ok());
    }

    #[test]
    fn test_validate_key_empty() {
        let result = CacheValidator::validate_key("");
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "Invalid key: key cannot be empty"
        );
    }

    #[test]
    fn test_validate_key_too_long() {
        let long_key = "a".repeat(513);
        let result = CacheValidator::validate_key(&long_key);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "Key too long: max 512 bytes");
    }

    #[test]
    fn test_validate_key_invalid_characters() {
        let result = CacheValidator::validate_key("key@with#invalid");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("invalid characters"));
    }

    #[test]
    fn test_validate_value_size_valid() {
        let value = vec![0u8; 1024]; // 1KB
        assert!(CacheValidator::validate_value_size(&value).is_ok());
    }

    #[test]
    fn test_validate_value_size_too_large() {
        let value = vec![0u8; 513 * 1024 * 1024]; // 513MB
        let result = CacheValidator::validate_value_size(&value);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "Value too large: max 512MB"
        );
    }

    #[test]
    fn test_validate_ttl_valid() {
        assert!(CacheValidator::validate_ttl(3600).is_ok());
        assert!(CacheValidator::validate_ttl(1).is_ok());
    }

    #[test]
    fn test_validate_ttl_invalid() {
        let result = CacheValidator::validate_ttl(0);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().to_string(), "Invalid TTL: must be positive");

        let result = CacheValidator::validate_ttl(-1);
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_entry_valid() {
        let result = CacheValidator::validate_entry("cache:key", b"value", Some(3600));
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_entry_invalid_key() {
        let result = CacheValidator::validate_entry("", b"value", Some(3600));
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_entry_invalid_ttl() {
        let result = CacheValidator::validate_entry("cache:key", b"value", Some(-1));
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_entry_no_ttl() {
        let result = CacheValidator::validate_entry("cache:key", b"value", None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_key_boundary_length() {
        let key_512 = "a".repeat(512);
        assert!(CacheValidator::validate_key(&key_512).is_ok());

        let key_513 = "a".repeat(513);
        assert!(CacheValidator::validate_key(&key_513).is_err());
    }

    #[test]
    fn test_validate_value_boundary_size() {
        let value_512mb = vec![0u8; 512 * 1024 * 1024];
        assert!(CacheValidator::validate_value_size(&value_512mb).is_ok());
    }
}
