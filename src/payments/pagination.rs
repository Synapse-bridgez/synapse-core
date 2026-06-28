//! Pagination support for payments queries with caching and optimization.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use utoipa::ToSchema;

/// Configuration for pagination caching.
#[derive(Debug, Clone)]
pub struct PaginationConfig {
    /// Default page size if not specified.
    pub default_page_size: u32,
    /// Maximum allowed page size to prevent unbounded queries.
    pub max_page_size: u32,
    /// Cache duration for total count queries (in seconds).
    pub count_cache_duration_secs: u64,
}

impl Default for PaginationConfig {
    fn default() -> Self {
        Self {
            default_page_size: 20,
            max_page_size: 100,
            count_cache_duration_secs: 30,
        }
    }
}

/// Pagination parameters for list queries.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PaginationParams {
    /// 1-based page number.
    pub page: u32,
    /// Number of records per page.
    pub page_size: u32,
}

impl PaginationParams {
    /// Create new pagination parameters, validating and clamping to safe defaults.
    pub fn new(page: u32, page_size: u32, config: &PaginationConfig) -> Result<Self, String> {
        if page < 1 {
            return Err("page must be >= 1".to_string());
        }
        if page_size < 1 {
            return Err("page_size must be >= 1".to_string());
        }
        if page_size > config.max_page_size {
            return Err(format!(
                "page_size {} exceeds maximum {}",
                page_size, config.max_page_size
            ));
        }
        Ok(PaginationParams { page, page_size })
    }

    /// Calculate the OFFSET for database queries.
    pub fn offset(&self) -> u32 {
        (self.page - 1) * self.page_size
    }

    /// Calculate the LIMIT for database queries.
    pub fn limit(&self) -> u32 {
        self.page_size
    }
}

/// Cached count value with timestamp.
#[derive(Debug, Clone)]
struct CachedCount {
    count: u64,
    cached_at: Instant,
}

impl CachedCount {
    fn is_expired(&self, config: &PaginationConfig) -> bool {
        self.cached_at.elapsed() > Duration::from_secs(config.count_cache_duration_secs)
    }
}

/// Manager for pagination with count caching.
pub struct PaginationManager {
    config: PaginationConfig,
    cached_counts: Arc<RwLock<std::collections::HashMap<String, CachedCount>>>,
}

impl PaginationManager {
    /// Create a new pagination manager with default configuration.
    pub fn new() -> Self {
        Self::with_config(PaginationConfig::default())
    }

    /// Create a new pagination manager with custom configuration.
    pub fn with_config(config: PaginationConfig) -> Self {
        Self {
            config,
            cached_counts: Arc::new(RwLock::new(std::collections::HashMap::new())),
        }
    }

    /// Get or compute the total count for a query, with caching.
    pub async fn get_cached_count(
        &self,
        cache_key: &str,
        compute_count: impl std::future::Future<Output = Result<u64, String>>,
    ) -> Result<u64, String> {
        // Check if we have a valid cached count
        let cached = self.cached_counts.read().await;
        if let Some(cached_count) = cached.get(cache_key) {
            if !cached_count.is_expired(&self.config) {
                return Ok(cached_count.count);
            }
        }
        drop(cached);

        // Compute and cache the count
        let count = compute_count.await?;
        let mut cache = self.cached_counts.write().await;
        cache.insert(
            cache_key.to_string(),
            CachedCount {
                count,
                cached_at: Instant::now(),
            },
        );

        Ok(count)
    }

    /// Invalidate the count cache for a specific key.
    pub async fn invalidate_cache(&self, cache_key: &str) {
        let mut cache = self.cached_counts.write().await;
        cache.remove(cache_key);
    }

    /// Invalidate all cached counts.
    pub async fn invalidate_all_cache(&self) {
        let mut cache = self.cached_counts.write().await;
        cache.clear();
    }
}

impl Default for PaginationManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Paginated response envelope.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PaginatedResponse<T> {
    /// The page of results.
    pub data: Vec<T>,
    /// Total number of records matching the query.
    pub total: u64,
    /// Current page number (1-based).
    pub page: u32,
    /// Page size used.
    pub page_size: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pagination_params_valid() {
        let config = PaginationConfig::default();
        let params = PaginationParams::new(1, 20, &config).unwrap();
        assert_eq!(params.page, 1);
        assert_eq!(params.page_size, 20);
        assert_eq!(params.offset(), 0);
        assert_eq!(params.limit(), 20);
    }

    #[test]
    fn test_pagination_params_invalid_page_zero() {
        let config = PaginationConfig::default();
        let result = PaginationParams::new(0, 20, &config);
        assert!(result.is_err());
    }

    #[test]
    fn test_pagination_params_invalid_page_size_zero() {
        let config = PaginationConfig::default();
        let result = PaginationParams::new(1, 0, &config);
        assert!(result.is_err());
    }

    #[test]
    fn test_pagination_params_page_size_exceeds_max() {
        let config = PaginationConfig::default();
        let result = PaginationParams::new(1, 200, &config);
        assert!(result.is_err());
    }

    #[test]
    fn test_pagination_offset_calculation() {
        let config = PaginationConfig::default();
        let params = PaginationParams::new(2, 10, &config).unwrap();
        assert_eq!(params.offset(), 10);
        assert_eq!(params.limit(), 10);
    }

    #[tokio::test]
    async fn test_count_caching() {
        let manager = PaginationManager::new();
        let mut call_count = 0;

        let count = manager
            .get_cached_count("test_key", async {
                call_count += 1;
                Ok::<u64, String>(42)
            })
            .await
            .unwrap();

        assert_eq!(count, 42);
        assert_eq!(call_count, 1);

        let count2 = manager
            .get_cached_count("test_key", async {
                call_count += 1;
                Ok::<u64, String>(99)
            })
            .await
            .unwrap();

        assert_eq!(count2, 42);
        assert_eq!(call_count, 1);
    }

    #[tokio::test]
    async fn test_cache_invalidation() {
        let manager = PaginationManager::new();
        let mut call_count = 0;

        manager
            .get_cached_count("test_key", async {
                call_count += 1;
                Ok::<u64, String>(42)
            })
            .await
            .unwrap();

        manager.invalidate_cache("test_key").await;

        let _count = manager
            .get_cached_count("test_key", async {
                call_count += 1;
                Ok::<u64, String>(99)
            })
            .await
            .unwrap();

        assert_eq!(call_count, 2);
    }
}
