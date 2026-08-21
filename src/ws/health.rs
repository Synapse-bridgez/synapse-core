//! Health checks for WebSocket connections.
//!
//! Health state is complementary to graceful shutdown. A draining server can
//! still mark an individual connection healthy while it sends final events and
//! a close frame. Handlers should mark unhealthy connections promptly so stale
//! sockets do not delay shutdown.
//!
//! # Status transitions
//!
//! ```text
//! Healthy ──► Degraded ──► Unhealthy
//!    ▲                         │
//!    └─────── mark_healthy ────┘
//! ```
//!
//! # Thread safety
//!
//! [`HealthChecker`] is `Send + Sync`. The health flags use `Acquire`/`Release`
//! ordering so that writes in one thread are immediately visible to readers in
//! other threads. [`RateLimiter`] is already lock-free (`Arc<Inner>` with
//! atomics), so no external `Mutex` is needed.
use crate::auth::input_validation::{validate_api_key, validate_auth_header};
use crate::cache::rate_limiting::RateLimiter;
use crate::validation::{sanitize_string, validate_required};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Health status of a WebSocket connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    Healthy,
    /// Connection is operational but experiencing elevated errors or latency.
    Degraded,
    Unhealthy,
}

/// Monitors health of WebSocket connections with security features.
///
/// Cloning is cheap — all clones share the same underlying state via `Arc`.
pub struct HealthChecker {
    /// `true` = healthy/degraded, `false` = unhealthy.
    is_healthy: Arc<AtomicBool>,
    /// `true` = degraded (only meaningful when `is_healthy` is `true`).
    is_degraded: Arc<AtomicBool>,
    last_check: Arc<parking_lot::Mutex<Instant>>,
    check_interval: Duration,
    /// Lock-free rate limiter — `RateLimiter` is `Send + Sync` via `Arc<Inner>`.
    rate_limiter: RateLimiter,
    /// Authentication key for health check access.
    auth_key: Option<String>,
}

impl HealthChecker {
    /// Create a new health checker with the specified check interval and optional auth key.
    pub fn new(check_interval: Duration, auth_key: Option<String>) -> Self {
        Self {
            is_healthy: Arc::new(AtomicBool::new(true)),
            is_degraded: Arc::new(AtomicBool::new(false)),
            last_check: Arc::new(parking_lot::Mutex::new(Instant::now())),
            check_interval,
            rate_limiter: RateLimiter::new(),
            auth_key,
        }
    }

    /// Returns `true` when the connection is healthy or degraded (not fully unhealthy).
    pub fn is_healthy(&self) -> bool {
        self.is_healthy.load(Ordering::Acquire)
    }

    /// Returns the three-tier health status.
    pub fn status(&self) -> HealthStatus {
        if !self.is_healthy.load(Ordering::Acquire) {
            return HealthStatus::Unhealthy;
        }
        if self.is_degraded.load(Ordering::Acquire) {
            return HealthStatus::Degraded;
        }
        HealthStatus::Healthy
    }

    /// Mark the connection as fully healthy, clearing any degraded flag.
    pub fn mark_healthy(&self) {
        self.is_degraded.store(false, Ordering::Release);
        self.is_healthy.store(true, Ordering::Release);
        *self.last_check.lock() = Instant::now();
    }

    /// Mark the connection as degraded (operational but impaired).
    pub fn mark_degraded(&self) {
        self.is_healthy.store(true, Ordering::Release);
        self.is_degraded.store(true, Ordering::Release);
    }

    /// Mark the connection as unhealthy.
    pub fn mark_unhealthy(&self) {
        self.is_healthy.store(false, Ordering::Release);
    }

    /// Returns `true` when a health check is due.
    pub fn should_check(&self) -> bool {
        self.last_check.lock().elapsed() >= self.check_interval
    }

    /// Returns the elapsed time since the last health check.
    pub fn time_since_last_check(&self) -> Duration {
        self.last_check.lock().elapsed()
    }

    /// Validate an `Authorization` header for health check access.
    ///
    /// Accepts `API-Key <key>` and `Bearer <token>` schemes.
    /// Returns `Ok(())` if authentication passes or no auth key is configured.
    pub fn validate_auth(&self, auth_header: &str) -> Result<(), String> {
        if let Some(ref key) = self.auth_key {
            if let Some(provided_key) = auth_header.strip_prefix("API-Key ") {
                validate_api_key(provided_key)?;
                if provided_key != key.as_str() {
                    return Err("Invalid API key".to_string());
                }
            } else if let Some(token) = auth_header.strip_prefix("Bearer ") {
                validate_auth_header(token)?;
            } else {
                return Err("Invalid authorization header format".to_string());
            }
        }
        Ok(())
    }

    /// Returns `true` if the request is within the configured rate limit.
    pub fn check_rate_limit(&self) -> bool {
        self.rate_limiter.try_acquire()
    }

    /// Validate and sanitize an arbitrary health-check input string.
    ///
    /// An empty string is accepted without further validation.
    pub fn validate_input(&self, input: &str) -> Result<(), String> {
        if !input.is_empty() {
            // Reject inputs containing control characters (e.g. null bytes) outright
            // rather than silently sanitizing them away — they indicate malformed or
            // potentially malicious input.
            if input.chars().any(|ch| ch.is_control()) {
                return Err("input contains invalid control characters".to_string());
            }
            let sanitized = sanitize_string(input);
            validate_required("health_input", &sanitized).map_err(|e| e.to_string())?;
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
    fn test_mark_degraded() {
        let checker = HealthChecker::new(Duration::from_secs(10), None);
        checker.mark_degraded();
        assert!(checker.is_healthy()); // degraded is still "up"
        assert_eq!(checker.status(), HealthStatus::Degraded);
    }

    #[test]
    fn test_mark_healthy_clears_degraded() {
        let checker = HealthChecker::new(Duration::from_secs(10), None);
        checker.mark_degraded();
        checker.mark_healthy();
        assert_eq!(checker.status(), HealthStatus::Healthy);
    }

    #[test]
    fn test_unhealthy_takes_priority_over_degraded() {
        let checker = HealthChecker::new(Duration::from_secs(10), None);
        checker.mark_degraded();
        checker.mark_unhealthy();
        assert_eq!(checker.status(), HealthStatus::Unhealthy);
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
        assert!(elapsed.as_millis() < 100);
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

        assert!(checker
            .validate_auth("API-Key test_api_key_1234567890123456789012")
            .is_ok());
        assert!(checker.validate_auth("API-Key invalid_key").is_err());
        assert!(checker.validate_auth("Bearer token123").is_err());
    }

    #[test]
    fn test_auth_validation_with_bearer_token() {
        let checker = HealthChecker::new(Duration::from_secs(10), None);

        // No auth_key configured — any header is accepted.
        assert!(checker.validate_auth("Bearer valid_token_123").is_ok());
    }

    #[test]
    fn test_auth_validation_bearer_empty_token() {
        let auth_key = "test_api_key_1234567890123456789012".to_string();
        let checker = HealthChecker::new(Duration::from_secs(10), Some(auth_key));

        // Empty token after "Bearer " should fail validation.
        assert!(checker.validate_auth("Bearer ").is_err());
    }

    #[test]
    fn test_rate_limiting() {
        let checker = HealthChecker::new(Duration::from_secs(10), None);
        assert!(checker.check_rate_limit());
        assert!(checker.check_rate_limit());
    }

    #[test]
    fn test_input_validation() {
        let checker = HealthChecker::new(Duration::from_secs(10), None);

        assert!(checker.validate_input("valid_input").is_ok());
        assert!(checker.validate_input("").is_ok());
        assert!(checker.validate_input("\u{0000}invalid").is_err());
    }

    #[test]
    fn test_concurrent_status_updates() {
        use std::sync::Arc;
        let checker = Arc::new(HealthChecker::new(Duration::from_secs(10), None));
        let mut handles = vec![];

        for i in 0..8 {
            let c = Arc::clone(&checker);
            handles.push(std::thread::spawn(move || {
                if i % 2 == 0 {
                    c.mark_unhealthy();
                } else {
                    c.mark_healthy();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        // After all threads finish the status must be one of the valid variants.
        let s = checker.status();
        assert!(matches!(s, HealthStatus::Healthy | HealthStatus::Unhealthy));
    }
}
