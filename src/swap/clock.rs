//! Clock-skew detection and guardrails for swap deadlines.
//!
//! Quote expiries and settlement deadlines are wall-clock based, so a drifting
//! replica can accept a quote another replica rejects. [`ClockGuard`] holds the
//! latest measured offset against a trusted time source; time-sensitive swap
//! operations consult it through [`ClockGuard::now`] / [`ClockGuard::check`].

use chrono::{DateTime, Duration, Utc};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;

/// Env var: max tolerated skew in milliseconds (default 1000).
pub const MAX_SKEW_ENV: &str = "SWAP_CLOCK_MAX_SKEW_MS";
const DEFAULT_MAX_SKEW_MS: i64 = 1_000;

/// A source of trusted time (e.g. an HTTP `Date` header or NTP client).
#[async_trait::async_trait]
pub trait TrustedTime: Send + Sync {
    async fn now(&self) -> anyhow::Result<DateTime<Utc>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClockSkewError {
    pub skew_ms: i64,
    pub max_skew_ms: i64,
}

impl std::fmt::Display for ClockSkewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "clock skew {}ms exceeds allowed {}ms; time-sensitive swap operations rejected",
            self.skew_ms, self.max_skew_ms
        )
    }
}

impl std::error::Error for ClockSkewError {}

/// Shared clock abstraction gating time-sensitive swap operations.
#[derive(Clone)]
pub struct ClockGuard {
    inner: Arc<Inner>,
}

struct Inner {
    /// local - trusted, in milliseconds.
    skew_ms: AtomicI64,
    /// True once at least one measurement succeeded; avoids startup false positives.
    measured: AtomicBool,
    max_skew_ms: i64,
}

impl ClockGuard {
    pub fn new(max_skew_ms: i64) -> Self {
        Self {
            inner: Arc::new(Inner {
                skew_ms: AtomicI64::new(0),
                measured: AtomicBool::new(false),
                max_skew_ms: max_skew_ms.max(0),
            }),
        }
    }

    pub fn from_env() -> Self {
        let max = std::env::var(MAX_SKEW_ENV)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_MAX_SKEW_MS);
        Self::new(max)
    }

    pub fn record_skew_ms(&self, skew_ms: i64) {
        self.inner.skew_ms.store(skew_ms, Ordering::Relaxed);
        self.inner.measured.store(true, Ordering::Relaxed);
    }

    pub fn skew_ms(&self) -> i64 {
        self.inner.skew_ms.load(Ordering::Relaxed)
    }

    /// Ok while skew is within bounds (or nothing measured yet, so startup is
    /// not flagged before the first probe completes).
    pub fn check(&self) -> Result<(), ClockSkewError> {
        let skew = self.skew_ms();
        if self.inner.measured.load(Ordering::Relaxed) && skew.abs() > self.inner.max_skew_ms {
            return Err(ClockSkewError {
                skew_ms: skew,
                max_skew_ms: self.inner.max_skew_ms,
            });
        }
        Ok(())
    }

    /// Readiness helper: false when skew exceeds the threshold.
    pub fn is_healthy(&self) -> bool {
        self.check().is_ok()
    }

    /// Local time corrected by the measured offset.
    pub fn now(&self) -> DateTime<Utc> {
        Utc::now() - Duration::milliseconds(self.skew_ms())
    }

    /// Whether `deadline` has passed per the skew-corrected clock. Errors when
    /// skew is out of bounds so callers degrade instead of mis-expiring quotes.
    pub fn is_expired(&self, deadline: DateTime<Utc>) -> Result<bool, ClockSkewError> {
        self.check()?;
        Ok(self.now() >= deadline)
    }

    /// Measure once against `source`, record the offset, and log context.
    pub async fn probe(&self, source: &dyn TrustedTime) -> anyhow::Result<i64> {
        let before = Utc::now();
        let trusted = source.now().await?;
        let after = Utc::now();
        // Compare against the midpoint of the request to cancel network latency.
        let local = before + (after - before) / 2;
        let skew = (local - trusted).num_milliseconds();
        self.record_skew_ms(skew);
        if let Err(e) = self.check() {
            tracing::warn!(
                skew_ms = skew,
                max_skew_ms = e.max_skew_ms,
                "swap clock skew exceeds threshold"
            );
        }
        Ok(skew)
    }

    /// Spawn the periodic probe. A failing time source keeps the last known
    /// skew (never flips health on source unavailability alone).
    pub fn spawn_monitor(
        &self,
        source: Arc<dyn TrustedTime>,
        interval: std::time::Duration,
    ) -> tokio::task::JoinHandle<()> {
        let guard = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            loop {
                tick.tick().await;
                if let Err(e) = guard.probe(source.as_ref()).await {
                    tracing::warn!(error = %e, "swap clock probe failed; keeping last skew");
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(DateTime<Utc>);
    #[async_trait::async_trait]
    impl TrustedTime for Fixed {
        async fn now(&self) -> anyhow::Result<DateTime<Utc>> {
            Ok(self.0)
        }
    }

    #[test]
    fn healthy_before_first_measurement() {
        assert!(ClockGuard::new(100).is_healthy());
    }

    #[test]
    fn threshold_boundary() {
        let g = ClockGuard::new(100);
        g.record_skew_ms(100);
        assert!(g.check().is_ok());
        g.record_skew_ms(-101);
        let err = g.check().unwrap_err();
        assert_eq!(err.skew_ms, -101);
        assert!(g.is_expired(Utc::now()).is_err());
    }

    #[test]
    fn now_applies_offset() {
        let g = ClockGuard::new(10_000);
        g.record_skew_ms(5_000);
        let diff = (Utc::now() - g.now()).num_milliseconds();
        assert!((4_900..=5_100).contains(&diff));
    }

    #[tokio::test]
    async fn probe_records_skew() {
        let g = ClockGuard::new(1_000);
        let skew = g
            .probe(&Fixed(Utc::now() - Duration::seconds(5)))
            .await
            .unwrap();
        assert!((4_900..=5_500).contains(&skew));
        assert!(!g.is_healthy());
    }
}
