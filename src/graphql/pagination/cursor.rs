//! Cursor-based pagination implementation

use crate::graphql::pagination::{DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE};
use base64::{engine::general_purpose::STANDARD, Engine};

/// Cursor-based pagination parameters
#[derive(Debug, Clone)]
pub struct CursorPagination {
    /// Cursor to start after (base64 encoded)
    pub after: Option<String>,
    /// Cursor to start before (base64 encoded)
    pub before: Option<String>,
    /// Maximum number of items to return
    pub first: Option<i64>,
    /// Maximum number of items to return (backwards)
    pub last: Option<i64>,
}

impl CursorPagination {
    /// Creates a new cursor pagination with validation
    pub fn new(
        after: Option<String>,
        before: Option<String>,
        first: Option<i64>,
        last: Option<i64>,
    ) -> Result<Self, String> {
        // Validate that both first and last are not specified
        if first.is_some() && last.is_some() {
            return Err("Cannot specify both 'first' and 'last'".to_string());
        }

        // Validate first/last values. Values exceeding MAX_PAGE_SIZE are not an
        // error — they are clamped to the maximum by `page_size()`.
        if let Some(f) = first {
            if f < 0 {
                return Err("'first' must be non-negative".to_string());
            }
        }

        if let Some(l) = last {
            if l < 0 {
                return Err("'last' must be non-negative".to_string());
            }
        }

        // Validate cursor format
        if let Some(ref a) = after {
            Self::validate_cursor(a)?;
        }
        if let Some(ref b) = before {
            Self::validate_cursor(b)?;
        }

        Ok(Self {
            after,
            before,
            first,
            last,
        })
    }

    /// Validates cursor format (must be valid base64)
    fn validate_cursor(cursor: &str) -> Result<(), String> {
        STANDARD
            .decode(cursor)
            .map_err(|_| "Invalid cursor format".to_string())?;
        Ok(())
    }

    /// Encodes a value as a cursor
    pub fn encode_cursor(value: &str) -> String {
        STANDARD.encode(value)
    }

    /// Decodes a cursor to its original value
    pub fn decode_cursor(cursor: &str) -> Result<String, String> {
        let decoded = STANDARD
            .decode(cursor)
            .map_err(|_| "Invalid cursor format".to_string())?;
        String::from_utf8(decoded).map_err(|_| "Cursor contains invalid UTF-8".to_string())
    }

    /// Gets the effective page size
    pub fn page_size(&self) -> i64 {
        self.first
            .or(self.last)
            .unwrap_or(DEFAULT_PAGE_SIZE)
            .min(MAX_PAGE_SIZE)
    }

    /// Checks if pagination is forward (using 'after')
    pub fn is_forward(&self) -> bool {
        self.first.is_some() || (self.last.is_none() && self.after.is_some())
    }

    /// Checks if pagination is backward (using 'before')
    pub fn is_backward(&self) -> bool {
        self.last.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_valid() {
        let pagination = CursorPagination::new(None, None, Some(20), None);
        assert!(pagination.is_ok());
    }

    #[test]
    fn test_both_first_and_last_error() {
        let pagination = CursorPagination::new(None, None, Some(20), Some(20));
        assert!(pagination.is_err());
    }

    #[test]
    fn test_negative_first_error() {
        let pagination = CursorPagination::new(None, None, Some(-1), None);
        assert!(pagination.is_err());
    }

    #[test]
    fn test_first_exceeds_max_is_clamped() {
        // Requesting more than the maximum is accepted and clamped, not rejected.
        let pagination = CursorPagination::new(None, None, Some(MAX_PAGE_SIZE + 1), None).unwrap();
        assert_eq!(pagination.page_size(), MAX_PAGE_SIZE);
    }

    #[test]
    fn test_encode_decode_cursor() {
        let original = "transaction:123";
        let encoded = CursorPagination::encode_cursor(original);
        let decoded = CursorPagination::decode_cursor(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_invalid_cursor_format() {
        let pagination = CursorPagination::new(Some("!!!invalid!!!".to_string()), None, None, None);
        assert!(pagination.is_err());
    }

    #[test]
    fn test_page_size_default() {
        let pagination = CursorPagination::new(None, None, None, None).unwrap();
        assert_eq!(pagination.page_size(), DEFAULT_PAGE_SIZE);
    }

    #[test]
    fn test_page_size_clamped() {
        let pagination = CursorPagination::new(None, None, Some(MAX_PAGE_SIZE + 50), None).unwrap();
        assert_eq!(pagination.page_size(), MAX_PAGE_SIZE);
    }

    #[test]
    fn test_is_forward() {
        let pagination = CursorPagination::new(None, None, Some(20), None).unwrap();
        assert!(pagination.is_forward());
    }

    #[test]
    fn test_is_backward() {
        let pagination = CursorPagination::new(None, None, None, Some(20)).unwrap();
        assert!(pagination.is_backward());
    }
}
