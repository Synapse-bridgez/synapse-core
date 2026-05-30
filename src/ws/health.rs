//! Health checks for WebSocket connections.
//!
//! Health state is complementary to graceful shutdown. A draining server can
//! still mark an individual connection healthy while it sends final events and
//! a close frame. Handlers should mark unhealthy connections promptly so stale
//! sockets do not delay shutdown.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use crate::cache::rate_limiting::RateLimiter;
use crate::auth::input_validation::{validate_api_key, validate_auth_header};
use crate::validation::{validate_required, sanitize_string};

/// Health status of a WebSocket connection
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    Healthy,
    Degraded,
    Unhealthy,
}

/// Monitors health of WebSocket connections with security features
pub struct HealthChecker {
    is_healthy: Arc<AtomicBool>,
    last_check: Arc<parking_lot::Mutex<Instant>>,
    check_interval: Duration,
    /// Rate limiter for health check requests
    rate_limiter: Arc<std::sync::Mutex<RateLimiter>>,
    /// Authentication key for health check access
    auth_key: Option<String>,
}

impl HealthChecker {
    /// Create a new health checker with specified check interval and security configuration
    pub fn new(check_interval: Duration, auth_key: Option<String>) -> Self {
        Self {
            is_healthy: Arc::new(AtomicBool::new(true)),
            last_check: Arc::new(parking_lot::Mutex::new(Instant::now())),
            check_interval,
            rate_limiter: Arc::new(std::sync::Mutex::new(RateLimiter::new())),
            auth_key,
        }
    }

    /// Check if connection is healthy
    pub fn is_healthy(&self) -> bool {
        self.is_healthy.load(Ordering::Relaxed)
    }

    /// Get current health status
    pub fn status(&self) -> HealthStatus {
        if self.is_healthy.load(Ordering::Relaxed) {
            HealthStatus::Healthy
        } else {
            HealthStatus::Unhealthy
        }
    }

    /// Mark connection as healthy
    pub fn mark_healthy(&self) {
        self.is_healthy.store(true, Ordering::Relaxed);
        *self.last_check.lock() = Instant::now();
    }

    /// Mark connection as unhealthy
    pub fn mark_unhealthy(&self) {
        self.is_healthy.store(false, Ordering::Relaxed);
    }

    /// Check if health check is due
    pub fn should_check(&self) -> bool {
        self.last_check.lock().elapsed() >= self.check_interval
    }

    /// Get time since last check
    pub fn time_since_last_check(&self) -> Duration {
        self.last_check.lock().elapsed()
    }

    /// Validate authentication for health check access
    /// Returns Ok(()) if authentication is valid, Err(String) otherwise
    pub fn validate_auth(&self, auth_header: &str) -> Result<(), String> {
        if let Some(ref key) = self.auth_key {
            // For API key based auth
            if auth_header.starts_with("API-Key ") {
                let provided_key = &auth_header[8..];
                validate_api_key(provided_key)?;
                if provided_key != key.as_str() {
                    return Err("Invalid API key".to_string());
                }
            } else if auth_header.starts_with("Bearer ") {
                // For token based auth
                let token = &auth_header[7..];
                validate_auth_header(auth_header)?;
            } else {
                return Err("Invalid authorization header format".to_string());
            }
        }
        Ok(())
    }

    /// Check rate limit for health check requests
    /// Returns true if request is allowed, false otherwise
    pub fn check_rate_limit(&self) -> bool {
        let mut limiter = self.rate_limiter.lock().unwrap();
        limiter.try_acquire()
    }

    /// Validate input parameters for health check
    /// Returns Ok(()) if input is valid, Err(String) otherwise
    pub fn validate_input(&self, input: &str) -> Result<(), String> {
        if !input.is_empty() {
            let sanitized = sanitize_string(input);
            validate_required("health_input", &sanitized)?;
        }
        Ok(())
    }
}

impl Default for HealthChecker {
    fn default() -> Self {
        Self::new(Duration::from_secs(30), None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_health_checker_creation() {
        let checker = HealthChecker::new(Duration::from_secs(10), None);
        assert!(checker.is_healthy());
        assert_eq!(checker.status(), HealthStatus::Healthy);
    }

    #[test]
    fn test_mark_unhealthy() {
        let checker = HealthChecker::new(Duration::from_secs(10), None);
        checker.mark_unhealthy();
        assert!(!checker.is_healthy());
        assert_eq!(checker.status(), HealthStatus::Unhealthy);
    }

    #[test]
    fn test_mark_healthy() {
        let checker = HealthChecker::new(Duration::from_secs(10), None);
        checker.mark_unhealthy();
        checker.mark_healthy();
        assert!(checker.is_healthy());
        assert_eq!(checker.status(), HealthStatus::Healthy);
    }

    #[test]
    fn test_should_check_initially_false() {
        let checker = HealthChecker::new(Duration::from_secs(10), None);
        assert!(!checker.should_check());
    }

    #[test]
    fn test_time_since_last_check() {
        let checker = HealthChecker::new(Duration::from_secs(10), None);
        let elapsed = checker.time_since_last_check();
        assert!(elapsed.as_millis() < 100); // Should be very recent
    }

    #[test]
    fn test_default_health_checker() {
        let checker = HealthChecker::default();
        assert!(checker.is_healthy());
        assert_eq!(checker.check_interval, Duration::from_secs(30));
    }

    #[test]
    fn test_auth_validation_with_api_key() {
        let auth_key = "test_api_key_1234567890123456789012".to_string();
        let checker = HealthChecker::new(Duration::from_secs(10), Some(auth_key.clone()));
        
        // Valid API key
        assert!(checker.validate_auth("API-Key test_api_key_1234567890123456789012").is_ok());
        
        // Invalid API key
        assert!(checker.validate_auth("API-Key invalid_key").is_err());
        
        // Wrong format
        assert!(checker.validate_auth("Bearer token123").is_err());
    }

    #[test]
    fn test_auth_validation_with_bearer_token() {
        let checker = HealthChecker::new(Duration::from_secs(10), None);
        
        // Valid bearer token
        assert!(checker.validate_auth("Bearer valid_token_123").is_ok());
        
        // Invalid bearer token
        assert!(checker.validate_auth("Bearer ").is_err());
    }

    #[test]
    fn test_rate_limiting() {
        let checker = HealthChecker::new(Duration::from_secs(10), None);
        
        // First request should be allowed
        assert!(checker.check_rate_limit());
        
        // Second request should be allowed (default limit is 100 per minute)
        assert!(checker.check_rate_limit());
    }

    #[test]
    fn test_input_validation() {
        let checker = HealthChecker::new(Duration::from_secs(10), None);
        
        // Valid input
        assert!(checker.validate_input("valid_input").is_ok());
        
        // Empty input
        assert!(checker.validate_input("").is_ok());
        
        // Input with control characters
        assert!(checker.validate_input("\u{0000}invalid").is_err());
    }
}
