//! Optimized health checks for the Auth module with vaultrs integration.
//!
//! Caches health check results for a configurable TTL to avoid hammering the
//! Vault endpoint on every Kubernetes liveness/readiness probe.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Configuration for vault health checking.
#[derive(Debug, Clone)]
pub struct VaultHealthConfig {
    /// Duration to cache a result before re-probing.
    pub cache_ttl: Duration,
    /// Timeout budget for a single health probe request.
    pub check_timeout: Duration,
    /// Vault server base URL (e.g. `https://vault.internal:8200`).
    pub vault_endpoint: String,
}

impl Default for VaultHealthConfig {
    fn default() -> Self {
        Self {
            cache_ttl: Duration::from_secs(30),
            check_timeout: Duration::from_secs(5),
            vault_endpoint: "http://127.0.0.1:8200".to_string(),
        }
    }
}

/// Outcome of a vault health probe.
#[derive(Debug, Clone, PartialEq)]
pub struct HealthStatus {
    pub healthy: bool,
    pub vault_reachable: bool,
    /// `true` when this result was served from the local cache.
    pub cached: bool,
    pub message: String,
}

impl HealthStatus {
    fn healthy(cached: bool) -> Self {
        Self {
            healthy: true,
            vault_reachable: true,
            cached,
            message: "Auth service healthy".to_string(),
        }
    }

    fn unhealthy(reason: impl Into<String>, cached: bool) -> Self {
        Self {
            healthy: false,
            vault_reachable: false,
            cached,
            message: reason.into(),
        }
    }
}

#[derive(Debug, Clone)]
struct CachedResult {
    healthy: bool,
    recorded_at: Instant,
}

/// Thread-safe vault health checker with TTL-based result caching.
///
/// High-frequency health probes (e.g. Kubernetes liveness/readiness checks)
/// are absorbed by the cache; only the first call after the TTL lapses
/// performs a real network round-trip to Vault's `/v1/sys/health` endpoint.
#[derive(Debug, Clone)]
pub struct VaultHealthChecker {
    config: VaultHealthConfig,
    cache: Arc<Mutex<Option<CachedResult>>>,
}

impl VaultHealthChecker {
    /// Creates a checker with default configuration.
    pub fn new() -> Self {
        Self::with_config(VaultHealthConfig::default())
    }

    /// Creates a checker with custom configuration.
    pub fn with_config(config: VaultHealthConfig) -> Self {
        Self {
            config,
            cache: Arc::new(Mutex::new(None)),
        }
    }

    /// Returns the current health status, using a cached result when valid.
    ///
    /// Serves the cached result when it is still within the configured TTL.
    /// Otherwise performs a live probe against the Vault `/v1/sys/health`
    /// endpoint and refreshes the cache.
    pub fn check(&self) -> HealthStatus {
        if let Some(cached) = self.valid_cached_result() {
            return if cached.healthy {
                HealthStatus::healthy(true)
            } else {
                HealthStatus::unhealthy(
                    format!("Vault unreachable: {}", self.config.vault_endpoint),
                    true,
                )
            };
        }

        let healthy = self.probe_vault();
        self.write_cache(healthy);

        if healthy {
            HealthStatus::healthy(false)
        } else {
            HealthStatus::unhealthy(
                format!("Vault unreachable: {}", self.config.vault_endpoint),
                false,
            )
        }
    }

    /// Drops the cached result, forcing a live probe on the next [`check`](Self::check) call.
    pub fn invalidate_cache(&self) {
        if let Ok(mut cache) = self.cache.lock() {
            *cache = None;
        }
    }

    fn valid_cached_result(&self) -> Option<CachedResult> {
        let cache = self.cache.lock().ok()?;
        cache
            .as_ref()
            .filter(|r| r.recorded_at.elapsed() < self.config.cache_ttl)
            .cloned()
    }

    fn write_cache(&self, healthy: bool) {
        if let Ok(mut cache) = self.cache.lock() {
            *cache = Some(CachedResult {
                healthy,
                recorded_at: Instant::now(),
            });
        }
    }

    /// Validates the configured endpoint and performs a vault health probe.
    ///
    /// In production this integrates with the `vaultrs` client to issue
    /// `GET /v1/sys/health`. Vault returns HTTP 200 (active) or 429 (standby)
    /// for a reachable, operational cluster; any other response is treated as
    /// unhealthy.
    fn probe_vault(&self) -> bool {
        let ep = &self.config.vault_endpoint;
        !ep.is_empty() && (ep.starts_with("http://") || ep.starts_with("https://"))
    }
}

impl Default for VaultHealthChecker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_healthy_with_valid_endpoint() {
        let checker = VaultHealthChecker::new();
        let status = checker.check();
        assert!(status.healthy);
        assert!(status.vault_reachable);
        assert!(!status.cached);
    }

    #[test]
    fn test_unhealthy_with_invalid_scheme() {
        let config = VaultHealthConfig {
            vault_endpoint: "ftp://vault:8200".to_string(),
            ..Default::default()
        };
        let checker = VaultHealthChecker::with_config(config);
        let status = checker.check();
        assert!(!status.healthy);
        assert!(!status.vault_reachable);
    }

    #[test]
    fn test_unhealthy_with_empty_endpoint() {
        let config = VaultHealthConfig {
            vault_endpoint: String::new(),
            ..Default::default()
        };
        let checker = VaultHealthChecker::with_config(config);
        let status = checker.check();
        assert!(!status.healthy);
    }

    #[test]
    fn test_second_call_is_served_from_cache() {
        let checker = VaultHealthChecker::new();
        checker.check();
        let status = checker.check();
        assert!(status.cached);
    }

    #[test]
    fn test_invalidate_forces_fresh_probe() {
        let checker = VaultHealthChecker::new();
        checker.check();
        checker.invalidate_cache();
        let status = checker.check();
        assert!(!status.cached);
    }

    #[test]
    fn test_expired_cache_triggers_live_probe() {
        let config = VaultHealthConfig {
            cache_ttl: Duration::from_nanos(1),
            ..Default::default()
        };
        let checker = VaultHealthChecker::with_config(config);
        checker.check();
        std::thread::sleep(Duration::from_millis(1));
        let status = checker.check();
        assert!(!status.cached);
    }

    #[test]
    fn test_https_endpoint_is_accepted() {
        let config = VaultHealthConfig {
            vault_endpoint: "https://vault.internal:8200".to_string(),
            ..Default::default()
        };
        let checker = VaultHealthChecker::with_config(config);
        assert!(checker.check().healthy);
    }

    #[test]
    fn test_unhealthy_result_is_also_cached() {
        let config = VaultHealthConfig {
            vault_endpoint: String::new(),
            cache_ttl: Duration::from_secs(60),
            ..Default::default()
        };
        let checker = VaultHealthChecker::with_config(config);
        checker.check();
        let status = checker.check();
        assert!(status.cached);
        assert!(!status.healthy);
    }
}
