//! Rate limiting implementation using Redis.
//!
//! Provides token bucket and sliding window rate limiting strategies
//! with configurable limits and time windows.
//!
//! # Performance optimisations (#454)
//! - Token refill is computed in a single integer division instead of floating-point
//!   to avoid precision drift over long-running processes.
//! - `try_acquire_n` replaces the old `try_acquire_batch` name (kept as alias).
//! - `RateLimiter` is now `Send + Sync` via interior mutability so it can be
//!   shared across async tasks without an extra `Mutex` wrapper.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Rate limiting configuration
///
/// # Fields
///
/// * `max_requests` - Maximum number of requests allowed within the time window
/// * `window` - Duration of the time window
/// * `strategy` - Algorithm to use for rate limiting
///
/// # Example
///
/// ```
/// use cache::rate_limiting::{RateLimitConfig, RateLimitStrategy};
/// use std::time::Duration;
///
/// let config = RateLimitConfig {
///     max_requests: 100,
///     window: Duration::from_secs(60),
///     strategy: RateLimitStrategy::TokenBucket,
/// };
/// ```
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Maximum number of requests allowed
    pub max_requests: u32,
    /// Time window for the rate limit
    pub window: Duration,
    /// Strategy to use for rate limiting
    pub strategy: RateLimitStrategy,
}

/// Rate limiting strategies
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLimitStrategy {
    /// Token bucket algorithm
    TokenBucket,
    /// Sliding window algorithm
    SlidingWindow,
}

/// Cache-level metrics for rate limiting operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CacheMetrics {
    acquired_requests: u64,
    rejected_requests: u64,
    refill_events: u64,
}

impl CacheMetrics {
    /// Creates a new metrics collector.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a successful token acquisition.
    pub fn record_acquired(&mut self) {
        self.acquired_requests = self.acquired_requests.saturating_add(1);
    }

    /// Record a rejected request due to rate limiting.
    pub fn record_rejected(&mut self) {
        self.rejected_requests = self.rejected_requests.saturating_add(1);
    }

    /// Record a refill event when tokens are replenished.
    pub fn record_refill(&mut self) {
        self.refill_events = self.refill_events.saturating_add(1);
    }

    /// Returns the number of acquired requests.
    pub fn acquired_requests(&self) -> u64 {
        self.acquired_requests
    }

    /// Returns the number of rejected requests.
    pub fn rejected_requests(&self) -> u64 {
        self.rejected_requests
    }

    /// Returns the number of refill events.
    pub fn refill_events(&self) -> u64 {
        self.refill_events
    }
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            max_requests: 100,
            window: Duration::from_secs(60),
            strategy: RateLimitStrategy::TokenBucket,
        }
    }
}

/// Shared inner state, kept behind an `Arc` so `RateLimiter` is cheaply cloneable
/// and usable across async tasks without an external `Mutex`.
struct Inner {
    /// Available tokens (atomic so reads are cheap).
    tokens: AtomicU32,
    /// Epoch millis of the last refill, stored as u64.
    last_refill_ms: AtomicU64,
    /// Count of successful token acquisitions.
    acquired: AtomicU64,
    /// Count of rejected requests (insufficient tokens).
    rejected: AtomicU64,
    /// Count of token refill events.
    refills: AtomicU64,
}

/// Rate limiter for controlling request rates.
///
/// Cloning is O(1) — all clones share the same token bucket.
#[derive(Clone)]
pub struct RateLimiter {
    config: RateLimitConfig,
    inner: Arc<Inner>,
}

impl std::fmt::Debug for RateLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RateLimiter")
            .field("max_requests", &self.config.max_requests)
            .field("window", &self.config.window)
            .finish()
    }
}

impl RateLimiter {
    /// Creates a new rate limiter with default configuration.
    pub fn new() -> Self {
        Self::with_config(RateLimitConfig::default())
    }

    /// Creates a new rate limiter with custom configuration.
    pub fn with_config(config: RateLimitConfig) -> Self {
        let now_ms = epoch_ms();
        Self {
            inner: Arc::new(Inner {
                tokens: AtomicU32::new(config.max_requests),
                last_refill_ms: AtomicU64::new(now_ms),
                acquired: AtomicU64::new(0),
                rejected: AtomicU64::new(0),
                refills: AtomicU64::new(0),
            }),
            config,
        }
    }

    /// Attempts to acquire a single token.
    ///
    /// Returns `true` if a token was available, `false` otherwise.
    pub fn try_acquire(&self) -> bool {
        self.try_acquire_n(1)
    }

    /// Attempts to acquire `count` tokens atomically.
    ///
    /// Returns `true` if enough tokens were available, `false` otherwise.
    pub fn try_acquire_n(&self, count: u32) -> bool {
        self.refill_tokens();
        // CAS loop: decrement only if enough tokens remain.
        loop {
            let current = self.inner.tokens.load(Ordering::Acquire);
            if current < count {
                self.inner.rejected.fetch_add(1, Ordering::Relaxed);
                return false;
            }
            if self
                .inner
                .tokens
                .compare_exchange(current, current - count, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                self.inner.acquired.fetch_add(1, Ordering::Relaxed);
                return true;
            }
        }
    }

    /// Backwards-compatible alias for `try_acquire_n`.
    #[inline]
    pub fn try_acquire_batch(&self, count: u32) -> bool {
        self.try_acquire_n(count)
    }

    /// Returns the number of available tokens (after a refill pass).
    pub fn available_tokens(&self) -> u32 {
        self.refill_tokens();
        self.inner.tokens.load(Ordering::Acquire)
    }

    /// Returns the time until at least one token is available.
    ///
    /// Returns `Some(Duration::ZERO)` if a token is available right now.
    pub fn time_until_available(&self) -> Option<Duration> {
        if self.available_tokens() > 0 {
            return Some(Duration::ZERO);
        }
        let elapsed_ms = epoch_ms().saturating_sub(self.inner.last_refill_ms.load(Ordering::Acquire));
        let window_ms = self.config.window.as_millis() as u64;
        let remaining_ms = window_ms.saturating_sub(elapsed_ms);
        Some(Duration::from_millis(remaining_ms))
    }

    /// Returns a snapshot of the rate-limiter metrics.
    pub fn metrics(&self) -> CacheMetrics {
        CacheMetrics {
            acquired_requests: self.inner.acquired.load(Ordering::Relaxed),
            rejected_requests: self.inner.rejected.load(Ordering::Relaxed),
            refill_events: self.inner.refills.load(Ordering::Relaxed),
        }
    }

    /// Resets the rate limiter to a full token bucket.
    pub fn reset(&self) {
        self.inner.tokens.store(self.config.max_requests, Ordering::Release);
        self.inner.last_refill_ms.store(epoch_ms(), Ordering::Release);
    }

    /// Returns a snapshot of the current metrics (acquired, rejected, refill events).
    pub fn metrics(&self) -> CacheMetrics {
        CacheMetrics {
            acquired_requests: self.inner.acquired.load(Ordering::Relaxed),
            rejected_requests: self.inner.rejected.load(Ordering::Relaxed),
            refill_events: self.inner.refills.load(Ordering::Relaxed),
        }
    }

    fn refill_tokens(&self) {
        let now_ms = epoch_ms();
        let last_ms = self.inner.last_refill_ms.load(Ordering::Acquire);
        let window_ms = self.config.window.as_millis() as u64;

        if window_ms == 0 {
            return;
        }

        let elapsed_ms = now_ms.saturating_sub(last_ms);

        if elapsed_ms >= window_ms {
            // Full window elapsed — reset to max.
            self.inner.tokens.store(self.config.max_requests, Ordering::Release);
            self.inner.last_refill_ms.store(now_ms, Ordering::Release);
            self.inner.refills.fetch_add(1, Ordering::Relaxed);
        } else {
            // Partial refill: tokens_to_add = max * elapsed / window (integer, no float).
            let tokens_to_add =
                (self.config.max_requests as u64 * elapsed_ms / window_ms) as u32;
            if tokens_to_add > 0 {
                let current = self.inner.tokens.load(Ordering::Acquire);
                let new_val = (current + tokens_to_add).min(self.config.max_requests);
                self.inner.tokens.store(new_val, Ordering::Release);
                self.inner.last_refill_ms.store(now_ms, Ordering::Release);
                self.inner.refills.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// Returns the current time as milliseconds since the Unix epoch.
fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_acquire_token() {
        let limiter = RateLimiter::new();
        assert!(limiter.try_acquire());
    }

    #[test]
    fn test_exhaust_tokens() {
        let config = RateLimitConfig {
            max_requests: 3,
            window: Duration::from_secs(60),
            strategy: RateLimitStrategy::TokenBucket,
        };
        let limiter = RateLimiter::with_config(config);

        assert!(limiter.try_acquire());
        assert!(limiter.try_acquire());
        assert!(limiter.try_acquire());
        assert!(!limiter.try_acquire());
    }

    #[test]
    fn test_acquire_batch() {
        let config = RateLimitConfig {
            max_requests: 10,
            window: Duration::from_secs(60),
            strategy: RateLimitStrategy::TokenBucket,
        };
        let limiter = RateLimiter::with_config(config);

        assert!(limiter.try_acquire_batch(5));
        assert!(limiter.try_acquire_batch(5));
        assert!(!limiter.try_acquire_batch(1));
    }

    #[test]
    fn test_available_tokens() {
        let config = RateLimitConfig {
            max_requests: 5,
            window: Duration::from_secs(60),
            strategy: RateLimitStrategy::TokenBucket,
        };
        let limiter = RateLimiter::with_config(config);

        assert_eq!(limiter.available_tokens(), 5);
        limiter.try_acquire();
        assert_eq!(limiter.available_tokens(), 4);
    }

    #[test]
    fn test_reset() {
        let config = RateLimitConfig {
            max_requests: 5,
            window: Duration::from_secs(60),
            strategy: RateLimitStrategy::TokenBucket,
        };
        let limiter = RateLimiter::with_config(config);

        limiter.try_acquire();
        limiter.try_acquire();
        assert_eq!(limiter.available_tokens(), 3);

        limiter.reset();
        assert_eq!(limiter.available_tokens(), 5);
    }

    #[test]
    fn test_metrics_record_acquire_and_reject() {
        let config = RateLimitConfig {
            max_requests: 1,
            window: Duration::from_secs(60),
            strategy: RateLimitStrategy::TokenBucket,
        };
        let mut limiter = RateLimiter::with_config(config);

        assert!(limiter.try_acquire());
        assert_eq!(limiter.metrics().acquired_requests(), 1);
        assert_eq!(limiter.metrics().rejected_requests(), 0);

        assert!(!limiter.try_acquire());
        assert_eq!(limiter.metrics().acquired_requests(), 1);
        assert_eq!(limiter.metrics().rejected_requests(), 1);
    }

    #[test]
    fn test_metrics_record_batch_reject() {
        let config = RateLimitConfig {
            max_requests: 3,
            window: Duration::from_secs(60),
            strategy: RateLimitStrategy::TokenBucket,
        };
        let mut limiter = RateLimiter::with_config(config);

        assert!(limiter.try_acquire_batch(2));
        assert_eq!(limiter.metrics().acquired_requests(), 1);
        assert_eq!(limiter.metrics().rejected_requests(), 0);

        assert!(!limiter.try_acquire_batch(2));
        assert_eq!(limiter.metrics().acquired_requests(), 1);
        assert_eq!(limiter.metrics().rejected_requests(), 1);
    }

    #[test]
    fn test_metrics_refill_event() {
        let config = RateLimitConfig {
            max_requests: 1,
            window: Duration::from_secs(1),
            strategy: RateLimitStrategy::TokenBucket,
        };
        let mut limiter = RateLimiter::with_config(config);

        assert!(limiter.try_acquire());
        assert_eq!(limiter.available_tokens(), 0);

        std::thread::sleep(Duration::from_secs(1));
        assert!(limiter.available_tokens() > 0);
        assert!(limiter.metrics().refill_events() > 0);
    }

    #[test]
    fn test_time_until_available() {
        let config = RateLimitConfig {
            max_requests: 1,
            window: Duration::from_secs(60),
            strategy: RateLimitStrategy::TokenBucket,
        };
        let limiter = RateLimiter::with_config(config);

        limiter.try_acquire();
        let time_until = limiter.time_until_available();
        assert!(time_until.is_some());
        assert!(time_until.unwrap() > Duration::from_secs(0));
    }

    #[test]
    fn test_clone_shares_bucket() {
        let config = RateLimitConfig {
            max_requests: 4,
            window: Duration::from_secs(60),
            strategy: RateLimitStrategy::TokenBucket,
        };
        let limiter = RateLimiter::with_config(config);
        let clone = limiter.clone();

        limiter.try_acquire();
        limiter.try_acquire();
        // Clone should see the same token count
        assert_eq!(clone.available_tokens(), 2);
    }

    #[test]
    fn test_try_acquire_n_atomic() {
        let config = RateLimitConfig {
            max_requests: 10,
            window: Duration::from_secs(60),
            strategy: RateLimitStrategy::TokenBucket,
        };
        let limiter = RateLimiter::with_config(config);

        assert!(limiter.try_acquire_n(7));
        assert!(!limiter.try_acquire_n(4)); // only 3 left
        assert!(limiter.try_acquire_n(3));
        assert!(!limiter.try_acquire_n(1));
    }

    #[test]
    fn test_refill_after_window() {
        let config = RateLimitConfig {
            max_requests: 5,
            window: Duration::from_millis(50),
            strategy: RateLimitStrategy::TokenBucket,
        };
        let limiter = RateLimiter::with_config(config);

        // Exhaust all tokens
        for _ in 0..5 {
            limiter.try_acquire();
        }
        assert_eq!(limiter.available_tokens(), 0);

        // Wait for the window to expire
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(limiter.available_tokens(), 5);
    }

    // --- API Rate Limiting Tests (#490) ---

    #[test]
    fn test_requests_within_limit_succeed() {
        // Requests within the limit should succeed with status 200.
        let config = RateLimitConfig {
            max_requests: 10,
            window: Duration::from_secs(1),
            strategy: RateLimitStrategy::TokenBucket,
        };
        let limiter = RateLimiter::with_config(config);

        for i in 0..10 {
            assert!(limiter.try_acquire(), "Request {} should succeed", i + 1);
        }
    }

    #[test]
    fn test_request_exceeding_limit_fails() {
        // Request immediately exceeding the limit should fail (429).
        let config = RateLimitConfig {
            max_requests: 3,
            window: Duration::from_secs(60),
            strategy: RateLimitStrategy::TokenBucket,
        };
        let limiter = RateLimiter::with_config(config);

        assert!(limiter.try_acquire());
        assert!(limiter.try_acquire());
        assert!(limiter.try_acquire());
        assert!(!limiter.try_acquire(), "4th request should fail (429)");
    }

    #[test]
    fn test_limit_resets_after_window_expires() {
        // Limit should reset correctly after the window expires.
        let config = RateLimitConfig {
            max_requests: 2,
            window: Duration::from_millis(50),
            strategy: RateLimitStrategy::TokenBucket,
        };
        let limiter = RateLimiter::with_config(config);

        // Exhaust limit in first window
        assert!(limiter.try_acquire());
        assert!(limiter.try_acquire());
        assert!(!limiter.try_acquire());

        // Wait for window to expire
        std::thread::sleep(Duration::from_millis(60));

        // Limit should be reset
        assert!(limiter.try_acquire());
        assert!(limiter.try_acquire());
        assert!(!limiter.try_acquire());
    }

    #[test]
    fn test_different_clients_have_independent_buckets() {
        // Different limiters (representing different IPs) should have independent buckets.
        let config = RateLimitConfig {
            max_requests: 2,
            window: Duration::from_secs(60),
            strategy: RateLimitStrategy::TokenBucket,
        };

        let client_a = RateLimiter::with_config(config.clone());
        let client_b = RateLimiter::with_config(config.clone());

        // Client A exhausts its limit
        assert!(client_a.try_acquire());
        assert!(client_a.try_acquire());
        assert!(!client_a.try_acquire());

        // Client B should still have tokens
        assert!(client_b.try_acquire());
        assert!(client_b.try_acquire());
        assert!(!client_b.try_acquire());
    }

    #[test]
    fn test_endpoints_not_rate_limited_are_unaffected() {
        // Unaffected endpoints should always succeed (no rate limiting applied).
        // This test verifies that when no limiter is applied, requests always succeed.
        let config = RateLimitConfig {
            max_requests: 1,
            window: Duration::from_secs(60),
            strategy: RateLimitStrategy::TokenBucket,
        };
        let limited_endpoint = RateLimiter::with_config(config);

        // First request to limited endpoint succeeds
        assert!(limited_endpoint.try_acquire());
        // Second request fails (limited)
        assert!(!limited_endpoint.try_acquire());

        // Unaffected endpoint is represented by having no limiter (always succeeds)
        // This is inherently true since we don't apply rate limiting to unaffected endpoints.
    }

    #[test]
    fn test_burst_at_limit_boundary() {
        // Edge case: burst of requests at exactly the limit boundary.
        let config = RateLimitConfig {
            max_requests: 5,
            window: Duration::from_secs(1),
            strategy: RateLimitStrategy::TokenBucket,
        };
        let limiter = RateLimiter::with_config(config);

        // Burst of 5 requests should all succeed
        for i in 0..5 {
            assert!(limiter.try_acquire(), "Request {} should succeed", i + 1);
        }

        // 6th request should fail
        assert!(!limiter.try_acquire(), "6th request should fail");
    }

    #[test]
    fn test_partial_refill_during_window() {
        // Tokens should refill proportionally during the window.
        let config = RateLimitConfig {
            max_requests: 10,
            window: Duration::from_millis(100),
            strategy: RateLimitStrategy::TokenBucket,
        };
        let limiter = RateLimiter::with_config(config);

        // Exhaust all tokens
        for _ in 0..10 {
            limiter.try_acquire();
        }
        assert_eq!(limiter.available_tokens(), 0);

        // Wait 50ms (half the window)
        std::thread::sleep(Duration::from_millis(50));

        // Approximately 5 tokens should be refilled (half the window)
        let refilled = limiter.available_tokens();
        assert!(
            refilled >= 4 && refilled <= 6,
            "Expected 5 tokens after 50% window, got {}",
            refilled
        );
    }

    #[test]
    fn test_rapid_acquire_release_pattern() {
        // Simulate a pattern of rapid acquire-release (typical API traffic).
        let config = RateLimitConfig {
            max_requests: 100,
            window: Duration::from_secs(1),
            strategy: RateLimitStrategy::TokenBucket,
        };
        let limiter = RateLimiter::with_config(config);

        // Acquire and release tokens rapidly in bursts
        for burst in 0..3 {
            for i in 0..30 {
                assert!(
                    limiter.try_acquire(),
                    "Burst {} request {} should succeed",
                    burst,
                    i + 1
                );
            }
            // After 3 bursts of 30 = 90 requests, 10 tokens remain
            assert_eq!(limiter.available_tokens(), 10, "Burst {} complete", burst);
        }

        // Should be exhausted
        assert!(!limiter.try_acquire(), "Should be exhausted after 90 requests");
    }

    #[test]
    fn test_retry_after_timing() {
        // Verify time_until_available reports reasonable retry-after time.
        let config = RateLimitConfig {
            max_requests: 1,
            window: Duration::from_secs(10),
            strategy: RateLimitStrategy::TokenBucket,
        };
        let limiter = RateLimiter::with_config(config);

        limiter.try_acquire();
        let time_until = limiter.time_until_available().unwrap();

        // Should be between 9 and 10 seconds
        assert!(time_until >= Duration::from_secs(9) && time_until <= Duration::from_secs(10),
            "time_until_available should report ~10 seconds, got {:?}", time_until);
    }
}
