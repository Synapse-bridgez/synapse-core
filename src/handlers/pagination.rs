//! API pagination support for list endpoints with consistent response envelope.

use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Query parameters for paginated endpoints.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct PaginationQuery {
    /// 1-based page number (default: 1).
    #[serde(default = "default_page")]
    pub page: u32,
    /// Number of records per page (default: 20, max: 100).
    #[serde(default = "default_page_size")]
    pub page_size: u32,
}

fn default_page() -> u32 {
    1
}

fn default_page_size() -> u32 {
    20
}

/// Configuration for API pagination.
#[derive(Debug, Clone)]
pub struct ApiPaginationConfig {
    /// Default page size.
    pub default_page_size: u32,
    /// Maximum allowed page size.
    pub max_page_size: u32,
}

impl Default for ApiPaginationConfig {
    fn default() -> Self {
        Self {
            default_page_size: 20,
            max_page_size: 100,
        }
    }
}

/// Validate pagination query parameters.
pub fn validate_pagination(
    query: &PaginationQuery,
    config: &ApiPaginationConfig,
) -> Result<(), (StatusCode, String)> {
    if query.page < 1 {
        return Err((StatusCode::BAD_REQUEST, "page must be >= 1".to_string()));
    }

    if query.page_size < 1 {
        return Err((
            StatusCode::BAD_REQUEST,
            "page_size must be >= 1".to_string(),
        ));
    }

    if query.page_size > config.max_page_size {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "page_size {} exceeds maximum {}",
                query.page_size, config.max_page_size
            ),
        ));
    }

    Ok(())
}

/// Generic paginated response envelope for API list endpoints.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PaginatedListResponse<T> {
    /// The page of results.
    pub data: Vec<T>,
    /// Total number of records matching the query.
    pub total: u64,
    /// Current page number (1-based).
    pub page: u32,
    /// Page size used in this response.
    pub page_size: u32,
}

impl<T> PaginatedListResponse<T> {
    /// Create a new paginated response.
    pub fn new(data: Vec<T>, total: u64, page: u32, page_size: u32) -> Self {
        Self {
            data,
            total,
            page,
            page_size,
        }
    }
}

/// Helper struct for managing pagination parameters and offsets.
pub struct PaginationHelper {
    page: u32,
    page_size: u32,
}

impl PaginationHelper {
    /// Create a new pagination helper from query parameters.
    pub fn from_query(
        query: &PaginationQuery,
        config: &ApiPaginationConfig,
    ) -> Result<Self, (StatusCode, String)> {
        validate_pagination(query, config)?;

        let page = query.page;
        let page_size = query.page_size.max(1).min(config.max_page_size);

        Ok(Self { page, page_size })
    }

    /// Get the OFFSET for database queries.
    pub fn offset(&self) -> u32 {
        (self.page - 1) * self.page_size
    }

    /// Get the LIMIT for database queries.
    pub fn limit(&self) -> u32 {
        self.page_size
    }

    /// Get the current page number.
    pub fn page(&self) -> u32 {
        self.page
    }

    /// Get the current page size.
    pub fn page_size(&self) -> u32 {
        self.page_size
    }

    /// Calculate the total number of pages for a given total count.
    pub fn total_pages(&self, total: u64) -> u32 {
        total.div_ceil(self.page_size as u64) as u32
    }

    /// Check if the current page extends past the available data.
    pub fn is_beyond_total(&self, total: u64) -> bool {
        self.offset() as u64 + self.page_size as u64 > total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_pagination_valid_default() {
        let query = PaginationQuery {
            page: 1,
            page_size: 20,
        };
        let config = ApiPaginationConfig::default();
        assert!(validate_pagination(&query, &config).is_ok());
    }

    #[test]
    fn test_validate_pagination_invalid_page_zero() {
        let query = PaginationQuery {
            page: 0,
            page_size: 20,
        };
        let config = ApiPaginationConfig::default();
        assert!(validate_pagination(&query, &config).is_err());
    }

    #[test]
    fn test_validate_pagination_invalid_page_size_zero() {
        let query = PaginationQuery {
            page: 1,
            page_size: 0,
        };
        let config = ApiPaginationConfig::default();
        assert!(validate_pagination(&query, &config).is_err());
    }

    #[test]
    fn test_validate_pagination_exceeds_max_page_size() {
        let query = PaginationQuery {
            page: 1,
            page_size: 200,
        };
        let config = ApiPaginationConfig::default();
        let result = validate_pagination(&query, &config);
        assert!(result.is_err());
        let (status, msg) = result.unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(msg.contains("exceeds maximum"));
    }

    #[test]
    fn test_pagination_helper_offset_calculation() {
        let query = PaginationQuery {
            page: 3,
            page_size: 10,
        };
        let config = ApiPaginationConfig::default();
        let helper = PaginationHelper::from_query(&query, &config).unwrap();
        assert_eq!(helper.offset(), 20);
        assert_eq!(helper.limit(), 10);
    }

    #[test]
    fn test_pagination_helper_total_pages() {
        let query = PaginationQuery {
            page: 1,
            page_size: 10,
        };
        let config = ApiPaginationConfig::default();
        let helper = PaginationHelper::from_query(&query, &config).unwrap();
        assert_eq!(helper.total_pages(0), 0);
        assert_eq!(helper.total_pages(10), 1);
        assert_eq!(helper.total_pages(11), 2);
        assert_eq!(helper.total_pages(25), 3);
    }

    #[test]
    fn test_pagination_helper_is_beyond_total() {
        let query = PaginationQuery {
            page: 5,
            page_size: 10,
        };
        let config = ApiPaginationConfig::default();
        let helper = PaginationHelper::from_query(&query, &config).unwrap();
        assert!(!helper.is_beyond_total(50));
        assert!(!helper.is_beyond_total(51));
        assert!(helper.is_beyond_total(49));
    }

    #[test]
    fn test_paginated_list_response() {
        let data = vec![1, 2, 3];
        let response: PaginatedListResponse<i32> = PaginatedListResponse::new(data, 100, 1, 20);
        assert_eq!(response.data.len(), 3);
        assert_eq!(response.total, 100);
        assert_eq!(response.page, 1);
        assert_eq!(response.page_size, 20);
    }
}
