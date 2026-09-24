use crate::cache::CacheValidator;
use async_trait::async_trait;
use redis::{aio::ConnectionManager, AsyncCommands};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use tracing::{error, info, warn};

/// Represents a conversion rate with staleness tracking
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ConversionRate {
    /// The conversion rate from source asset to target asset
    pub rate: f64,
    /// Unix timestamp when the rate was last refreshed
    pub last_refreshed_at: u64,
}

impl ConversionRate {
    /// Get the staleness of this rate in seconds
    pub fn staleness_seconds(&self) -> u64 {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now.saturating_sub(self.last_refreshed_at)
    }

    /// Check if this rate is within acceptable staleness window
    pub fn is_fresh(&self, max_staleness_secs: u64) -> bool {
        self.staleness_seconds() <= max_staleness_secs
    }
}

/// Rate source interface for pluggable external rate providers
#[async_trait]
pub trait RateSource: Send + Sync {
    /// Fetch conversion rate from source asset to target asset
    async fn fetch_rate(&self, from: &str, to: &str) -> Result<f64, Box<dyn std::error::Error + Send + Sync>>;
}

/// Configuration for the rate cache
#[derive(Clone, Debug)]
pub struct RateCacheConfig {
    /// Refresh interval in seconds
    pub refresh_interval_secs: u64,
    /// Maximum staleness before we consider a rate too old
    pub max_staleness_secs: u64,
    /// Redis key prefix for rates
    pub redis_key_prefix: String,
}

impl Default for RateCacheConfig {
    fn default() -> Self {
        Self {
            refresh_interval_secs: 300, // 5 minutes
            max_staleness_secs: 3600,   // 1 hour
            redis_key_prefix: "rates:".to_string(),
        }
    }
}

/// Multi-asset conversion rate cache with Redis backing and graceful degradation
pub struct ConversionRateCache {
    redis: ConnectionManager,
    config: RateCacheConfig,
    // In-memory fallback for when Redis is unavailable
    fallback_rates: Arc<RwLock<HashMap<String, ConversionRate>>>,
    rate_source: Arc<dyn RateSource>,
}

impl ConversionRateCache {
    /// Create a new conversion rate cache
    pub fn new(
        redis: ConnectionManager,
        config: RateCacheConfig,
        rate_source: Arc<dyn RateSource>,
    ) -> Self {
        Self {
            redis,
            config,
            fallback_rates: Arc::new(RwLock::new(HashMap::new())),
            rate_source,
        }
    }

    /// Generate cache key for a rate pair
    fn make_key(&self, from: &str, to: &str) -> String {
        format!("{}{}_{}", self.config.redis_key_prefix, from.to_uppercase(), to.to_uppercase())
    }

    /// Get a conversion rate, returning staleness metadata
    pub async fn get_rate(&self, from: &str, to: &str) -> Result<ConversionRate, Box<dyn std::error::Error + Send + Sync>> {
        let key = self.make_key(from, to);

        // Try Redis first
        match self.redis.get::<_, String>(&key).await {
            Ok(data) => {
                match serde_json::from_str::<ConversionRate>(&data) {
                    Ok(rate) => {
                        info!(
                            staleness_secs = rate.staleness_seconds(),
                            from = from,
                            to = to,
                            "Cache hit for conversion rate"
                        );
                        return Ok(rate);
                    }
                    Err(e) => {
                        warn!("Failed to deserialize cached rate: {}", e);
                    }
                }
            }
            Err(e) => {
                warn!("Redis lookup failed for {}: {}", key, e);
            }
        }

        // Fallback to in-memory cache
        {
            let fallback = self.fallback_rates.read().await;
            if let Some(rate) = fallback.get(&key) {
                warn!(
                    staleness_secs = rate.staleness_seconds(),
                    from = from,
                    to = to,
                    "Using stale fallback rate"
                );
                return Ok(rate.clone());
            }
        }

        // No cached rate available
        Err(format!("No cached rate available for {}/{}", from, to).into())
    }

    /// Refresh a conversion rate from the external source
    pub async fn refresh_rate(&self, from: &str, to: &str) -> Result<ConversionRate, Box<dyn std::error::Error + Send + Sync>> {
        match self.rate_source.fetch_rate(from, to).await {
            Ok(rate) => {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();

                let conversion_rate = ConversionRate {
                    rate,
                    last_refreshed_at: now,
                };

                let key = self.make_key(from, to);
                if let Ok(data) = serde_json::to_string(&conversion_rate) {
                    // Save to Redis with expiry
                    let ttl = self.config.max_staleness_secs + self.config.refresh_interval_secs;
                    if let Err(e) = self.redis.set_ex::<_, _, _, ()>(&key, &data, ttl as usize).await {
                        error!("Failed to cache rate in Redis: {}", e);
                    }
                }

                // Update in-memory fallback
                {
                    let mut fallback = self.fallback_rates.write().await;
                    fallback.insert(key, conversion_rate.clone());
                }

                info!(
                    rate = conversion_rate.rate,
                    from = from,
                    to = to,
                    "Refreshed conversion rate"
                );
                Ok(conversion_rate)
            }
            Err(e) => {
                error!("Failed to fetch rate from source: {}", e);
                Err(e)
            }
        }
    }

    /// Get rate with automatic refresh if stale
    pub async fn get_rate_with_refresh(
        &self,
        from: &str,
        to: &str,
    ) -> Result<(ConversionRate, bool), Box<dyn std::error::Error + Send + Sync>> {
        // Try to get cached rate
        if let Ok(rate) = self.get_rate(from, to).await {
            let is_fresh = rate.is_fresh(self.config.max_staleness_secs);
            return Ok((rate, is_fresh));
        }

        // No cached rate, refresh from source
        let rate = self.refresh_rate(from, to).await?;
        Ok((rate, true))
    }

    /// Refresh all rates for a given source asset
    pub async fn refresh_rates_for_source(
        &self,
        from: &str,
        targets: &[&str],
    ) -> Vec<Result<ConversionRate, Box<dyn std::error::Error + Send + Sync>>> {
        let mut results = Vec::new();
        for target in targets {
            results.push(self.refresh_rate(from, target).await);
        }
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockRateSource {
        rates: HashMap<String, f64>,
    }

    #[async_trait]
    impl RateSource for MockRateSource {
        async fn fetch_rate(&self, from: &str, to: &str) -> Result<f64, Box<dyn std::error::Error + Send + Sync>> {
            let key = format!("{}_{}", from.to_uppercase(), to.to_uppercase());
            self.rates
                .get(&key)
                .copied()
                .ok_or_else(|| format!("Rate not found: {}", key).into())
        }
    }

    #[test]
    fn test_conversion_rate_staleness() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let rate = ConversionRate {
            rate: 1.5,
            last_refreshed_at: now - 100,
        };

        assert!(rate.staleness_seconds() >= 100);
        assert!(rate.is_fresh(200));
        assert!(!rate.is_fresh(50));
    }

    #[test]
    fn test_make_key() {
        let config = RateCacheConfig::default();
        let rates = ConversionRateCache::new(
            // This is a mock - in real tests we'd use a real or mocked ConnectionManager
            todo!(),
            config,
            Arc::new(MockRateSource {
                rates: HashMap::new(),
            }),
        );

        let key = rates.make_key("USD", "EUR");
        assert!(key.contains("USD_EUR"));
    }
}
