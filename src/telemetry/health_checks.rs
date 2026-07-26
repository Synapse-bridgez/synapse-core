//! Secure health check logic with input validation, response redaction, and call-frequency caching.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use utoipa::ToSchema;

/// Configuration for health check caching and rate limiting.
#[derive(Debug, Clone)]
pub struct HealthCheckConfig {
    /// Cache duration for health check results (in seconds).
    pub cache_duration_secs: u64,
    /// Maximum number of health checks per interval before caching is applied.
    pub max_uncached_checks_per_interval: u32,
}

impl Default for HealthCheckConfig {
    fn default() -> Self {
        Self {
            cache_duration_secs: 5,
            max_uncached_checks_per_interval: 2,
        }
    }
}

/// Cached health check result.
#[derive(Debug, Clone)]
struct CachedHealth {
    result: HealthCheckResult,
    cached_at: Instant,
}

impl CachedHealth {
    fn is_expired(&self, config: &HealthCheckConfig) -> bool {
        self.cached_at.elapsed() > Duration::from_secs(config.cache_duration_secs)
    }
}

/// Result of a health check, with sensitive values redacted.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HealthCheckResult {
    pub status: String,
    pub timestamp: u64,
    pub components: HealthComponents,
}

/// Individual health components (sensitive values redacted).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HealthComponents {
    pub database: ComponentStatus,
    pub telemetry_export: ComponentStatus,
    pub message_queue: ComponentStatus,
}

/// Status of a single component.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ComponentStatus {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl ComponentStatus {
    pub fn healthy() -> Self {
        Self {
            status: "healthy".to_string(),
            message: None,
        }
    }

    pub fn unhealthy(msg: impl Into<String>) -> Self {
        Self {
            status: "unhealthy".to_string(),
            message: Some(msg.into()),
        }
    }
}

/// Trait for types that can report the live status of a single service component.
///
/// Implement this trait and pass the boxed checker to
/// [`HealthCheckManager::with_component_checker`] so that `perform_health_check`
/// interrogates real component state instead of returning a hard-coded "healthy".
pub trait ComponentChecker: Send + Sync {
    /// Return the current [`ComponentStatus`] for this component.
    ///
    /// Implementations should be fast and non-blocking where possible; the
    /// health-check cache will shield the system from pathological call rates.
    fn check(&self) -> ComponentStatus;
}

/// Bundled component checkers, one per [`HealthComponents`] field.
struct ComponentCheckers {
    database: Box<dyn ComponentChecker>,
    telemetry_export: Box<dyn ComponentChecker>,
    message_queue: Box<dyn ComponentChecker>,
}

/// Manager for secure health checks with caching and rate limiting.
pub struct HealthCheckManager {
    config: HealthCheckConfig,
    cached_result: Arc<RwLock<Option<CachedHealth>>>,
    check_count: Arc<RwLock<u32>>,
    check_interval_start: Arc<RwLock<Instant>>,
    /// Optional real component checkers injected by the caller.
    ///
    /// When `None`, `perform_health_check` reports every component as "healthy"
    /// (baseline/placeholder behaviour).  Supply real checkers via
    /// [`HealthCheckManager::with_component_checker`] so that the manager
    /// reflects actual component state.
    component_checkers: Option<Arc<ComponentCheckers>>,
}

impl HealthCheckManager {
    /// Create a new health check manager with default configuration.
    ///
    /// No component checkers are injected; [`check`](Self::check) will return a
    /// hard-coded "healthy" baseline until real checkers are supplied via
    /// [`with_component_checker`](Self::with_component_checker).
    pub fn new() -> Self {
        Self::with_config(HealthCheckConfig::default())
    }

    /// Create a new health check manager with custom configuration.
    pub fn with_config(config: HealthCheckConfig) -> Self {
        Self {
            config,
            cached_result: Arc::new(RwLock::new(None)),
            check_count: Arc::new(RwLock::new(0)),
            check_interval_start: Arc::new(RwLock::new(Instant::now())),
            component_checkers: None,
        }
    }

    /// Attach real component checkers so that `perform_health_check` reflects
    /// actual component state rather than always returning "healthy".
    ///
    /// # Arguments
    ///
    /// - `database` — checks the database / connection-pool liveness.
    /// - `telemetry_export` — checks the OTLP export pipeline.
    /// - `message_queue` — checks the message queue / broker connectivity.
    pub fn with_component_checker(
        mut self,
        database: Box<dyn ComponentChecker>,
        telemetry_export: Box<dyn ComponentChecker>,
        message_queue: Box<dyn ComponentChecker>,
    ) -> Self {
        self.component_checkers = Some(Arc::new(ComponentCheckers {
            database,
            telemetry_export,
            message_queue,
        }));
        self
    }

    /// Validate health check request (ensure parameters are non-empty and well-formed).
    fn validate_request(&self, check_type: &str) -> Result<(), String> {
        if check_type.is_empty() {
            return Err("Health check type cannot be empty".to_string());
        }

        if check_type.len() > 256 {
            return Err("Health check type exceeds maximum length".to_string());
        }

        // Ensure check_type contains only alphanumeric characters and underscores
        if !check_type.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Err("Health check type contains invalid characters".to_string());
        }

        Ok(())
    }

    /// Check if we need to reset the check interval counter.
    async fn check_and_reset_interval(&self) {
        let mut interval_start = self.check_interval_start.write().await;
        if interval_start.elapsed() > Duration::from_secs(self.config.cache_duration_secs) {
            *interval_start = Instant::now();
            *self.check_count.write().await = 0;
        }
    }

    /// Perform a health check with caching and rate limiting.
    pub async fn check(&self, check_type: &str) -> Result<HealthCheckResult, String> {
        // Validate input
        self.validate_request(check_type)?;

        // Check if we have a cached result and haven't exceeded rate limit
        self.check_and_reset_interval().await;

        let cached = self.cached_result.read().await;
        if let Some(cached_health) = cached.as_ref() {
            if !cached_health.is_expired(&self.config) {
                return Ok(cached_health.result.clone());
            }
        }
        drop(cached);

        // Check if we should return cached result due to rate limiting
        let mut check_count = self.check_count.write().await;
        *check_count += 1;

        if *check_count > self.config.max_uncached_checks_per_interval {
            // Return cached result if available, even if expired
            let cached = self.cached_result.read().await;
            if let Some(cached_health) = cached.as_ref() {
                return Ok(cached_health.result.clone());
            }
        }
        drop(check_count);

        // Perform actual health check
        let result = self.perform_health_check().await?;

        // Cache the result
        let mut cache = self.cached_result.write().await;
        *cache = Some(CachedHealth {
            result: result.clone(),
            cached_at: Instant::now(),
        });

        Ok(result)
    }

    /// Run the real component checks (or fall back to the baseline if no
    /// checkers have been injected).
    ///
    /// When [`with_component_checker`](Self::with_component_checker) has been
    /// called the component statuses reflect live state; otherwise every
    /// component is reported as "healthy" (placeholder baseline — suitable only
    /// until real checkers are wired in).
    ///
    /// The top-level `status` field is "healthy" when all components are
    /// healthy and "degraded" when at least one reports unhealthy.
    async fn perform_health_check(&self) -> Result<HealthCheckResult, String> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "Failed to get current timestamp".to_string())?
            .as_millis() as u64;

        let components = if let Some(checkers) = &self.component_checkers {
            HealthComponents {
                database: checkers.database.check(),
                telemetry_export: checkers.telemetry_export.check(),
                message_queue: checkers.message_queue.check(),
            }
        } else {
            // No real checkers injected — return a static baseline.
            // Wire real checkers via `with_component_checker` to reflect actual state.
            HealthComponents {
                database: ComponentStatus::healthy(),
                telemetry_export: ComponentStatus::healthy(),
                message_queue: ComponentStatus::healthy(),
            }
        };

        let all_healthy = [
            &components.database,
            &components.telemetry_export,
            &components.message_queue,
        ]
        .iter()
        .all(|c| c.status == "healthy");

        Ok(HealthCheckResult {
            status: if all_healthy {
                "healthy".to_string()
            } else {
                "degraded".to_string()
            },
            timestamp: now,
            components,
        })
    }
}

impl Default for HealthCheckManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_valid_health_check() {
        let manager = HealthCheckManager::new();
        let result = manager.check("database").await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().status, "healthy");
    }

    #[tokio::test]
    async fn test_sensitive_values_not_in_response() {
        let manager = HealthCheckManager::new();
        let result = manager.check("database").await.unwrap();
        let json = serde_json::to_string(&result).unwrap();

        // Ensure sensitive values are not exposed
        assert!(!json.contains("endpoint"));
        assert!(!json.contains("credentials"));
        assert!(!json.contains("password"));
    }

    #[tokio::test]
    async fn test_rapid_calls_return_cached_result() {
        let manager = HealthCheckManager::new();

        let result1 = manager.check("database").await.unwrap();
        let timestamp1 = result1.timestamp;

        tokio::time::sleep(Duration::from_millis(10)).await;

        let result2 = manager.check("database").await.unwrap();
        let timestamp2 = result2.timestamp;

        // Timestamps should be identical due to caching
        assert_eq!(timestamp1, timestamp2);
    }

    #[tokio::test]
    async fn test_invalid_empty_check_type() {
        let manager = HealthCheckManager::new();
        let result = manager.check("").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("cannot be empty"));
    }

    #[tokio::test]
    async fn test_invalid_check_type_too_long() {
        let manager = HealthCheckManager::new();
        let long_type = "a".repeat(300);
        let result = manager.check(&long_type).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("exceeds maximum length"));
    }

    #[tokio::test]
    async fn test_invalid_check_type_special_chars() {
        let manager = HealthCheckManager::new();
        let result = manager.check("database@!$%").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid characters"));
    }
}
