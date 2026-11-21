//! Offset-based pagination implementation

use crate::graphql::pagination::{DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE};

/// Offset-based pagination parameters
#[derive(Debug, Clone)]
pub struct OffsetPagination {
    /// Number of items to skip
    pub offset: i64,
    /// Maximum number of items to return
    pub limit: i64,
}

impl OffsetPagination {
    /// Creates a new offset pagination with validation
    ///
    /// # Arguments
    /// * `offset` - Number of items to skip (will be clamped to >= 0)
    /// * `limit` - Maximum items to return (will be clamped to 1..MAX_PAGE_SIZE)
    pub fn new(offset: Option<i64>, limit: Option<i64>) -> Self {
        let offset = offset.unwrap_or(0).max(0);
        let limit = limit.unwrap_or(DEFAULT_PAGE_SIZE).clamp(1, MAX_PAGE_SIZE);

        Self { offset, limit }
    }

    /// Creates offset pagination from an already-decoded cursor offset.
    ///
    /// The cursor is decoded once by the caller; this helper reuses the
    /// decoded value instead of re-parsing the raw cursor string.
    pub fn from_decoded_cursor(decoded_offset: i64, limit: Option<i64>) -> Self {
        Self::new(Some(decoded_offset), limit)
    }

    /// Returns the SQL OFFSET value
    pub fn sql_offset(&self) -> i64 {
        self.offset
    }

    /// Returns the SQL LIMIT value
    pub fn sql_limit(&self) -> i64 {
        self.limit
    }

    /// Calculates the next page's offset
    pub fn next_offset(&self) -> i64 {
        self.offset + self.limit
    }

    /// Calculates the previous page's offset
    pub fn prev_offset(&self) -> i64 {
        (self.offset - self.limit).max(0)
    }

    /// Checks if there might be a next page (requires total count)
    pub fn has_next_page(&self, total_count: i64) -> bool {
        self.offset + self.limit < total_count
    }

    /// Checks if there is a previous page
    pub fn has_prev_page(&self) -> bool {
        self.offset > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_with_defaults() {
        let pagination = OffsetPagination::new(None, None);
        assert_eq!(pagination.offset, 0);
        assert_eq!(pagination.limit, DEFAULT_PAGE_SIZE);
    }

    #[test]
    fn test_clamps_limit_to_max() {
        let pagination = OffsetPagination::new(None, Some(200));
        assert_eq!(pagination.limit, MAX_PAGE_SIZE);
    }

    #[test]
    fn test_clamps_limit_to_min() {
        let pagination = OffsetPagination::new(None, Some(0));
        assert_eq!(pagination.limit, 1);
    }

    #[test]
    fn test_clamps_offset_to_non_negative() {
        let pagination = OffsetPagination::new(Some(-10), None);
        assert_eq!(pagination.offset, 0);
    }

    #[test]
    fn test_from_decoded_cursor_reuses_offset() {
        let pagination = OffsetPagination::from_decoded_cursor(40, Some(20));
        assert_eq!(pagination.offset, 40);
        assert_eq!(pagination.limit, 20);
    }

    #[test]
    fn test_from_decoded_cursor_clamps_negative() {
        let pagination = OffsetPagination::from_decoded_cursor(-5, None);
        assert_eq!(pagination.offset, 0);
        assert_eq!(pagination.limit, DEFAULT_PAGE_SIZE);
    }

    #[test]
    fn test_next_offset() {
        let pagination = OffsetPagination::new(Some(10), Some(20));
        assert_eq!(pagination.next_offset(), 30);
    }

    #[test]
    fn test_prev_offset() {
        let pagination = OffsetPagination::new(Some(30), Some(20));
        assert_eq!(pagination.prev_offset(), 10);
    }

    #[test]
    fn test_prev_offset_at_start() {
        let pagination = OffsetPagination::new(Some(0), Some(20));
        assert_eq!(pagination.prev_offset(), 0);
    }

    #[test]
    fn test_has_next_page() {
        let pagination = OffsetPagination::new(Some(0), Some(20));
        assert!(pagination.has_next_page(100));
        assert!(!pagination.has_next_page(20));
    }

    #[test]
    fn test_has_prev_page() {
        let pagination = OffsetPagination::new(Some(0), Some(20));
        assert!(!pagination.has_prev_page());

        let pagination = OffsetPagination::new(Some(20), Some(20));
        assert!(pagination.has_prev_page());
    }
}
