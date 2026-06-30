//! Input validation for Redis-backed cache operations.
//!
//! [`CacheValidator`] enforces size and format constraints on cache keys, values,
//! TTLs, and invalidation patterns before they reach Redis. Call these checks at
//! cache boundaries (for example [`QueryCache::get`](crate::services::query_cache::QueryCache::get))
//! so untrusted or malformed inputs are rejected without issuing Redis commands.
//!
//! # Limits
//!
//! | Input | Constraint | Rationale |
//! |-------|------------|-----------|
//! | Key | 1–512 bytes, `[A-Za-z0-9_:-]` | Prevents oversized keys and injection via key namespace |
//! | Value | ≤ 512 MiB | Bounds memory and network use per entry |
//! | TTL | Positive `i64` seconds | Ensures `SET EX` receives a valid expiry |
//! | Pattern | Same charset as keys plus optional trailing `*` | Safe `KEYS` / scan-style invalidation |
//!
//! # Security
//!
//! - Rejects empty keys and keys with characters outside the allowlist (e.g. spaces, `.`, `@`, `#`).
//! - Caps key and value size before serialization or Redis I/O.
//! - Rejects non-positive TTL to avoid ambiguous expiry behavior.
//!
//! # Performance
//!
//! Validation is O(key length) for keys and O(1) for value size (length check only).
//! No allocations occur on the success path beyond error strings for failures.

/// Maximum cache key length in bytes (UTF-8).
pub const MAX_KEY_LENGTH: usize = 512;

/// Maximum cache value size in bytes (512 MiB).
pub const MAX_VALUE_SIZE: usize = 512 * 1024 * 1024;

/// Validates cache keys, values, TTLs, and invalidation patterns.
#[derive(Debug, Clone)]
pub struct CacheValidator;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ValidationError {
    #[error("Invalid key: {0}")]
    InvalidKey(String),
    #[error("Invalid value: {0}")]
    InvalidValue(String),
    #[error("Invalid pattern: {0}")]
    InvalidPattern(String),
    #[error("Key too long: max 512 bytes")]
    KeyTooLong,
    #[error("Value too large: max 512MB")]
    ValueTooLarge,
    #[error("Invalid TTL: must be positive")]
    InvalidTTL,
}

impl CacheValidator {
    /// Returns `true` if `c` is allowed in a cache key or pattern (excluding `*`).
    #[inline]
    fn is_key_char(c: char) -> bool {
        c.is_ascii_alphanumeric() || c == '_' || c == ':' || c == '-'
    }

    /// Validates a cache key for format and length.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidKey`] when the key is empty or contains
    /// disallowed characters, or [`ValidationError::KeyTooLong`] when it exceeds
    /// [`MAX_KEY_LENGTH`].
    ///
    /// # Examples
    ///
    /// ```
    /// use synapse_core::cache::validation::CacheValidator;
    ///
    /// assert!(CacheValidator::validate_key("query:status_counts").is_ok());
    /// assert!(CacheValidator::validate_key("").is_err());
    /// ```
    pub fn validate_key(key: &str) -> Result<(), ValidationError> {
        if key.is_empty() {
            return Err(ValidationError::InvalidKey(
                "key cannot be empty".to_string(),
            ));
        }

        if key.len() > MAX_KEY_LENGTH {
            return Err(ValidationError::KeyTooLong);
        }

        if !key.chars().all(Self::is_key_char) {
            return Err(ValidationError::InvalidKey(
                "key contains invalid characters".to_string(),
            ));
        }

        Ok(())
    }

    /// Validates an invalidation pattern used with Redis `KEYS`-style scans.
    ///
    /// Patterns use the same character set as keys, with an optional single `*`
    /// suffix (e.g. `query:daily_totals:*`). A pattern without `*` must still be
    /// a valid key.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidPattern`] for malformed patterns, or
    /// [`ValidationError::KeyTooLong`] when the pattern exceeds [`MAX_KEY_LENGTH`].
    pub fn validate_pattern(pattern: &str) -> Result<(), ValidationError> {
        if pattern.is_empty() {
            return Err(ValidationError::InvalidPattern(
                "pattern cannot be empty".to_string(),
            ));
        }

        if pattern.len() > MAX_KEY_LENGTH {
            return Err(ValidationError::KeyTooLong);
        }

        let core = pattern.strip_suffix('*').unwrap_or(pattern);
        if pattern.contains('*') && core.len() + 1 != pattern.len() {
            return Err(ValidationError::InvalidPattern(
                "wildcard '*' is only allowed as a single trailing character".to_string(),
            ));
        }

        if core.is_empty() {
            return Err(ValidationError::InvalidPattern(
                "pattern cannot consist of only '*'".to_string(),
            ));
        }

        if !core.chars().all(Self::is_key_char) {
            return Err(ValidationError::InvalidPattern(
                "pattern contains invalid characters".to_string(),
            ));
        }

        Ok(())
    }

    /// Validates the serialized size of a cache value.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::ValueTooLarge`] when `value` exceeds [`MAX_VALUE_SIZE`].
    pub fn validate_value_size(value: &[u8]) -> Result<(), ValidationError> {
        if value.len() > MAX_VALUE_SIZE {
            return Err(ValidationError::ValueTooLarge);
        }
        Ok(())
    }

    /// Validates TTL in seconds for `SET EX` operations.
    ///
    /// # Errors
    ///
    /// Returns [`ValidationError::InvalidTTL`] when `ttl` is zero or negative.
    pub fn validate_ttl(ttl: i64) -> Result<(), ValidationError> {
        if ttl <= 0 {
            return Err(ValidationError::InvalidTTL);
        }
        Ok(())
    }

    /// Validates a key, value, and optional TTL before cache storage.
    ///
    /// When `ttl` is `None`, TTL is not checked (caller may use a default elsewhere).
    ///
    /// # Errors
    ///
    /// Returns the first validation error from key, value, or TTL checks.
    pub fn validate_entry(
        key: &str,
        value: &[u8],
        ttl: Option<i64>,
    ) -> Result<(), ValidationError> {
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
        assert!(CacheValidator::validate_key("query:status_counts").is_ok());
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
        let long_key = "a".repeat(MAX_KEY_LENGTH + 1);
        let result = CacheValidator::validate_key(&long_key);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            format!("Key too long: max {MAX_KEY_LENGTH} bytes")
        );
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
    fn test_validate_key_rejects_dot_space_and_unicode() {
        assert!(CacheValidator::validate_key("key.with.dot").is_err());
        assert!(CacheValidator::validate_key("key with space").is_err());
        assert!(CacheValidator::validate_key("cache:ключ").is_err());
    }

    #[test]
    fn test_validate_pattern_valid() {
        assert!(CacheValidator::validate_pattern("query:status_counts").is_ok());
        assert!(CacheValidator::validate_pattern("query:daily_totals:*").is_ok());
    }

    #[test]
    fn test_validate_pattern_invalid_wildcard() {
        assert!(CacheValidator::validate_pattern("query:*:totals").is_err());
        assert!(CacheValidator::validate_pattern("query:totals**").is_err());
        assert!(CacheValidator::validate_pattern("*").is_err());
        assert!(CacheValidator::validate_pattern("").is_err());
    }

    #[test]
    fn test_validate_pattern_rejects_key_invalid_characters() {
        assert!(CacheValidator::validate_pattern("query:daily totals:*").is_err());
        assert!(CacheValidator::validate_pattern("query.daily_totals:*").is_err());
    }

    #[test]
    fn test_validate_value_size_valid() {
        let value = vec![0u8; 1024]; // 1KB
        assert!(CacheValidator::validate_value_size(&value).is_ok());
    }

    #[test]
    fn test_validate_value_size_limit_constant() {
        assert_eq!(MAX_VALUE_SIZE, 512 * 1024 * 1024);
    }

    #[test]
    #[ignore = "Allocates ~513MB; run with cargo test -- --ignored"]
    fn test_validate_value_size_too_large() {
        let value = vec![0u8; MAX_VALUE_SIZE + 1];
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
        assert_eq!(
            result.unwrap_err().to_string(),
            "Invalid TTL: must be positive"
        );

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
        let key_512 = "a".repeat(MAX_KEY_LENGTH);
        assert!(CacheValidator::validate_key(&key_512).is_ok());

        let key_513 = "a".repeat(MAX_KEY_LENGTH + 1);
        assert!(CacheValidator::validate_key(&key_513).is_err());
    }

    #[test]
    #[ignore = "Allocates 512MB; run with cargo test -- --ignored"]
    fn test_validate_value_boundary_size() {
        let value_512mb = vec![0u8; MAX_VALUE_SIZE];
        assert!(CacheValidator::validate_value_size(&value_512mb).is_ok());
    }
}
