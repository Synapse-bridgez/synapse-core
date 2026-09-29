use crate::services::{ConversionRateCache, Job};
use async_trait::async_trait;
use std::sync::Arc;
use tracing::{error, info};

/// Background job for refreshing conversion rates
pub struct RateCacheRefreshJob {
    cache: Arc<ConversionRateCache>,
    // Configurable list of asset pairs to refresh
    asset_pairs: Vec<(String, String)>,
}

impl RateCacheRefreshJob {
    pub fn new(cache: Arc<ConversionRateCache>, asset_pairs: Vec<(String, String)>) -> Self {
        Self { cache, asset_pairs }
    }
}

#[async_trait]
impl Job for RateCacheRefreshJob {
    fn name(&self) -> &str {
        "rate_cache_refresh"
    }

    fn schedule(&self) -> &str {
        "0 */5 * * * *" // Every 5 minutes
    }

    async fn execute(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        info!("Starting conversion rate cache refresh");

        let mut total_refreshed = 0;
        let mut total_failed = 0;

        for (from, to) in &self.asset_pairs {
            match self.cache.refresh_rate(from, to).await {
                Ok(rate) => {
                    total_refreshed += 1;
                    info!(
                        rate = rate.rate,
                        from = from,
                        to = to,
                        "Successfully refreshed rate"
                    );
                }
                Err(e) => {
                    total_failed += 1;
                    error!(
                        error = %e,
                        from = from,
                        to = to,
                        "Failed to refresh rate (will use fallback)"
                    );
                }
            }
        }

        info!(
            total_refreshed = total_refreshed,
            total_failed = total_failed,
            "Completed conversion rate cache refresh"
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_job_schedule() {
        let job = RateCacheRefreshJob::new(todo!(), vec![]);
        assert_eq!(job.schedule(), "0 */5 * * * *");
        assert_eq!(job.name(), "rate_cache_refresh");
    }
}
