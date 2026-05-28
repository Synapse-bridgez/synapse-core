/// Health checks for WebSocket connections
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Health status of a WebSocket connection
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthStatus {
    Healthy,
    Degraded,
    Unhealthy,
}

/// Monitors health of WebSocket connections
pub struct HealthChecker {
    is_healthy: Arc<AtomicBool>,
    last_check: Arc<parking_lot::Mutex<Instant>>,
    check_interval: Duration,
}

impl HealthChecker {
    /// Create a new health checker with specified check interval
    pub fn new(check_interval: Duration) -> Self {
        Self {
            is_healthy: Arc::new(AtomicBool::new(true)),
            last_check: Arc::new(parking_lot::Mutex::new(Instant::now())),
            check_interval,
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
}

impl Default for HealthChecker {
    fn default() -> Self {
        Self::new(Duration::from_secs(30))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_health_checker_creation() {
        let checker = HealthChecker::new(Duration::from_secs(10));
        assert!(checker.is_healthy());
        assert_eq!(checker.status(), HealthStatus::Healthy);
    }

    #[test]
    fn test_mark_unhealthy() {
        let checker = HealthChecker::new(Duration::from_secs(10));
        checker.mark_unhealthy();
        assert!(!checker.is_healthy());
        assert_eq!(checker.status(), HealthStatus::Unhealthy);
    }

    #[test]
    fn test_mark_healthy() {
        let checker = HealthChecker::new(Duration::from_secs(10));
        checker.mark_unhealthy();
        checker.mark_healthy();
        assert!(checker.is_healthy());
        assert_eq!(checker.status(), HealthStatus::Healthy);
    }

    #[test]
    fn test_should_check_initially_false() {
        let checker = HealthChecker::new(Duration::from_secs(10));
        assert!(!checker.should_check());
    }

    #[test]
    fn test_time_since_last_check() {
        let checker = HealthChecker::new(Duration::from_secs(10));
        let elapsed = checker.time_since_last_check();
        assert!(elapsed.as_millis() < 100); // Should be very recent
    }

    #[test]
    fn test_default_health_checker() {
        let checker = HealthChecker::default();
        assert!(checker.is_healthy());
        assert_eq!(checker.check_interval, Duration::from_secs(30));
    }
}
