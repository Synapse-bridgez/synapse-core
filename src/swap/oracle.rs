//! Multi-source oracle quorum and failover handling for swap pricing (#1462).
//!
//! A [`PriceSource`] is one redundant feed of an asset pair's price. The
//! [`OracleQuorum`] queries every configured source, discards unavailable and
//! stale observations, and only returns a usable price when at least
//! `min_agreeing_sources` observations agree within `tolerance_bps`.
//!
//! When the quorum cannot be met the aggregator degrades instead of serving an
//! unverified price: it widens the spread, restricts quoting to a single
//! source, or halts quoting entirely, and emits an [`OracleEvent`] so operators
//! can alert on failover and divergence.

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use sqlx::types::BigDecimal;
use std::fmt;
use std::sync::Arc;

/// Default staleness threshold, in seconds, for oracle observations.
///
/// Preserves the historical hard-coded behaviour; operators can override it via
/// the `SWAP_ORACLE_MAX_STALENESS_SECS` configuration key.
pub const DEFAULT_MAX_STALENESS_SECS: i64 = 30;

/// One observation returned by a [`PriceSource`].
#[derive(Debug, Clone, PartialEq)]
pub struct PriceObservation {
    /// Source identifier, for logs/metrics.
    pub source: String,
    /// Mid price of the pair, expressed as quote-per-base.
    pub price: BigDecimal,
    /// When the source says the price was last updated.
    pub observed_at: DateTime<Utc>,
}

/// A redundant price feed for a single asset pair.
#[async_trait]
pub trait PriceSource: Send + Sync {
    /// Short identifier for logs/metrics (e.g. `"primary"`, `"secondary"`).
    fn name(&self) -> &str;

    /// Fetch the current observation. Errors mean the source is unavailable.
    async fn fetch(&self, base: &str, quote: &str) -> Result<PriceObservation, PriceSourceError>;
}

/// Error returned by a [`PriceSource`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PriceSourceError {
    /// The source could not be reached or timed out.
    #[error("price source unavailable: {0}")]
    Unavailable(String),
    /// The source answered but the payload was unusable.
    #[error("price source returned an invalid price: {0}")]
    Invalid(String),
}

/// A fixed-price adapter, useful for the primary feed and for tests.
#[derive(Debug, Clone)]
pub struct StaticPriceSource {
    name: String,
    price: BigDecimal,
}

impl StaticPriceSource {
    /// Build a source that always reports `price`.
    pub fn new(name: impl Into<String>, price: BigDecimal) -> Self {
        Self {
            name: name.into(),
            price,
        }
    }
}

#[async_trait]
impl PriceSource for StaticPriceSource {
    fn name(&self) -> &str {
        &self.name
    }

    async fn fetch(&self, _base: &str, _quote: &str) -> Result<PriceObservation, PriceSourceError> {
        Ok(PriceObservation {
            source: self.name.clone(),
            price: self.price.clone(),
            observed_at: Utc::now(),
        })
    }
}

/// A fallback adapter that delegates to an inner source and reports it under a
/// different name, so a secondary/backup feed can be wired without a new type.
#[derive(Debug, Clone)]
pub struct FallbackPriceSource {
    name: String,
    inner: Arc<dyn PriceSource>,
}

impl FallbackPriceSource {
    /// Wrap `inner` and report it as `name`.
    pub fn new(name: impl Into<String>, inner: Arc<dyn PriceSource>) -> Self {
        Self {
            name: name.into(),
            inner,
        }
    }
}

#[async_trait]
impl PriceSource for FallbackPriceSource {
    fn name(&self) -> &str {
        &self.name
    }

    async fn fetch(&self, base: &str, quote: &str) -> Result<PriceObservation, PriceSourceError> {
        let mut obs = self.inner.fetch(base, quote).await?;
        obs.source = self.name.clone();
        Ok(obs)
    }
}

/// Quorum configuration.
#[derive(Debug, Clone)]
pub struct QuorumConfig {
    /// Minimum number of sources that must agree for a price to be usable.
    pub min_agreeing_sources: usize,
    /// Maximum relative deviation, in basis points, between agreeing sources.
    pub tolerance_bps: u32,
    /// Observations older than this are treated as stale.
    pub max_staleness: Duration,
    /// Spread widening applied in degraded mode, in basis points.
    pub degraded_spread_widen_bps: u32,
}

impl Default for QuorumConfig {
    fn default() -> Self {
        Self {
            min_agreeing_sources: 2,
            tolerance_bps: 50,
            max_staleness: Duration::seconds(DEFAULT_MAX_STALENESS_SECS),
            degraded_spread_widen_bps: 100,
        }
    }
}

impl QuorumConfig {
    /// Build a config from the operator-supplied staleness threshold, in
    /// seconds, keeping the other quorum defaults.
    ///
    /// `max_staleness_secs` is expected to be validated at startup (positive
    /// and numeric); a non-positive value falls back to the documented default
    /// so the aggregator never treats every observation as stale.
    pub fn with_max_staleness_secs(max_staleness_secs: i64) -> Self {
        let secs = if max_staleness_secs > 0 {
            max_staleness_secs
        } else {
            DEFAULT_MAX_STALENESS_SECS
        };
        Self {
            max_staleness: Duration::seconds(secs),
            ..Self::default()
        }
    }
}

/// Degraded-mode behaviour when the quorum cannot be met.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DegradedMode {
    /// Serve the best single source with a widened spread.
    WidenSpread { widen_bps: u32 },
    /// Serve the best single source but restrict quoting to it.
    Restrict,
    /// Do not quote at all.
    Halt,
}

/// Why the aggregator degraded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DegradedReason {
    /// Too few sources answered.
    Unavailable,
    /// Sources answered but their observations were too old.
    Stale,
    /// Enough sources answered but they disagreed beyond tolerance.
    Divergent,
}

/// Event emitted on failover or divergence.
#[derive(Debug, Clone, PartialEq)]
pub enum OracleEvent {
    /// The quorum was met; `sources` are the agreeing sources.
    QuorumMet { sources: Vec<String> },
    /// The aggregator fell back to degraded mode.
    Failover {
        reason: DegradedReason,
        mode: DegradedMode,
        sources: Vec<String>,
    },
    /// Sources disagreed beyond tolerance.
    Divergence {
        spread_bps: u32,
        sources: Vec<String>,
    },
}

/// Result of a quorum evaluation.
#[derive(Debug, Clone, PartialEq)]
pub struct QuorumOutcome {
    /// The usable price, if any.
    pub price: Option<BigDecimal>,
    /// Sources that contributed to `price`.
    pub sources: Vec<String>,
    /// Degraded mode applied, if the quorum was not met.
    pub degraded: Option<DegradedMode>,
    /// Events emitted during this evaluation.
    pub events: Vec<OracleEvent>,
}

/// Aggregates several [`PriceSource`]s under a [`QuorumConfig`].
pub struct OracleQuorum {
    sources: Vec<Arc<dyn PriceSource>>,
    config: QuorumConfig,
}

impl fmt::Debug for OracleQuorum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OracleQuorum")
            .field("sources", &self.sources.len())
            .field("config", &self.config)
            .finish()
    }
}

impl OracleQuorum {
    /// Build an aggregator over `sources`.
    pub fn new(sources: Vec<Arc<dyn PriceSource>>, config: QuorumConfig) -> Self {
        Self { sources, config }
    }

    /// Query every source and evaluate the quorum for `base`/`quote`.
    pub async fn quote(&self, base: &str, quote: &str) -> QuorumOutcome {
        let now = Utc::now();
        let mut fresh: Vec<PriceObservation> = Vec::new();
        let mut stale = false;

        for source in &self.sources {
            match source.fetch(base, quote).await {
                Ok(obs) if now - obs.observed_at <= self.config.max_staleness => fresh.push(obs),
                Ok(_) => stale = true,
                Err(err) => {
                    tracing::warn!(source = source.name(), error = %err, "price source failed");
                }
            }
        }

        if fresh.len() >= self.config.min_agreeing_sources {
            if let Some(agreeing) = self.agreeing_group(&fresh) {
                let sources = agreeing.iter().map(|o| o.source.clone()).collect();
                return QuorumOutcome {
                    price: Some(median(&agreeing)),
                    sources: sources.clone(),
                    degraded: None,
                    events: vec![OracleEvent::QuorumMet { sources }],
                };
            }
        }

        let reason = if fresh.len() < self.config.min_agreeing_sources {
            if stale {
                DegradedReason::Stale
            } else {
                DegradedReason::Unavailable
            }
        } else {
            DegradedReason::Divergent
        };

        let mut events = Vec::new();
        if reason == DegradedReason::Divergent {
            let spread_bps = self.spread_bps(&fresh);
            events.push(OracleEvent::Divergence {
                spread_bps,
                sources: fresh.iter().map(|o| o.source.clone()).collect(),
            });
        }

        let mode = self.degraded_mode(reason);
        events.push(OracleEvent::Failover {
            reason,
            mode,
            sources: fresh.iter().map(|o| o.source.clone()).collect(),
        });

        let price = match mode {
            DegradedMode::Halt => None,
            DegradedMode::WidenSpread { .. } | DegradedMode::Restrict => {
                fresh.iter().max_by(|a, b| a.price.cmp(&b.price)).map(|o| o.price.clone())
            }
        };

        QuorumOutcome {
            price,
            sources: fresh.iter().map(|o| o.source.clone()).collect(),
            degraded: Some(mode),
            events,
        }
    }

    fn degraded_mode(&self, reason: DegradedReason) -> DegradedMode {
        match reason {
            DegradedReason::Divergent => DegradedMode::WidenSpread {
                widen_bps: self.config.degraded_spread_widen_bps,
            },
            DegradedReason::Stale | DegradedReason::Unavailable => DegradedMode::Restrict,
        }
    }

    fn agreeing_group(&self, fresh: &[PriceObservation]) -> Option<Vec<PriceObservation>> {
        let mut sorted = fresh.to_vec();
        sorted.sort_by(|a, b| a.price.cmp(&b.price));
        let tolerance = BigDecimal::from(self.config.tolerance_bps);
        let base = BigDecimal::from(10_000u32);
        for window in sorted.windows(self.config.min_agreeing_sources) {
            let low = &window[0].price;
            let high = &window[window.len() - 1].price;
            if low.is_zero() {
                continue;
            }
            let diff = (high - low).abs();
            let spread_bps = (&diff * &base) / low;
            if spread_bps <= tolerance {
                return Some(window.to_vec());
            }
        }
        None
    }

    fn spread_bps(&self, fresh: &[PriceObservation]) -> u32 {
        let mut prices: Vec<&BigDecimal> = fresh.iter().map(|o| &o.price).collect();
        prices.sort();
        let (Some(low), Some(high)) = (prices.first(), prices.last()) else {
            return 0;
        };
        if low.is_zero() {
            return 0;
        }
        let diff = (*high - *low).abs();
        let base = BigDecimal::from(10_000u32);
        let spread = (&diff * &base) / *low;
        spread.to_u32().unwrap_or(u32::MAX)
    }
}

fn median(observations: &[PriceObservation]) -> BigDecimal {
    let mut prices: Vec<&BigDecimal> = observations.iter().map(|o| &o.price).collect();
    prices.sort();
    let mid = prices.len() / 2;
    if prices.len() % 2 == 0 {
        let a = prices[mid - 1];
        let b = prices[mid];
        (a + b) / BigDecimal::from(2u32)
    } else {
        prices[mid].clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    /// A source that reports a fixed price with a fixed observation time.
    struct FixedTimeSource {
        name: String,
        price: BigDecimal,
        observed_at: DateTime<Utc>,
    }

    #[async_trait]
    impl PriceSource for FixedTimeSource {
        fn name(&self) -> &str {
            &self.name
        }

        async fn fetch(
            &self,
            _base: &str,
            _quote: &str,
        ) -> Result<PriceObservation, PriceSourceError> {
            Ok(PriceObservation {
                source: self.name.clone(),
                price: self.price.clone(),
                observed_at: self.observed_at,
            })
        }
    }

    fn source(name: &str, price: &str, age_secs: i64) -> Arc<dyn PriceSource> {
        Arc::new(FixedTimeSource {
            name: name.to_string(),
            price: BigDecimal::from_str(price).unwrap(),
            observed_at: Utc::now() - Duration::seconds(age_secs),
        })
    }

    #[test]
    fn default_preserves_historical_threshold() {
        assert_eq!(
            QuorumConfig::default().max_staleness,
            Duration::seconds(DEFAULT_MAX_STALENESS_SECS)
        );
        assert_eq!(DEFAULT_MAX_STALENESS_SECS, 30);
    }

    #[test]
    fn with_max_staleness_secs_uses_configured_value() {
        let config = QuorumConfig::with_max_staleness_secs(120);
        assert_eq!(config.max_staleness, Duration::seconds(120));
    }

    #[tokio::test]
    async fn feed_older_than_configured_threshold_is_rejected() {
        let config = QuorumConfig::with_max_staleness_secs(10);
        let quorum = OracleQuorum::new(
            vec![
                source("primary", "100", 30),
                source("secondary", "100", 30),
            ],
            config,
        );
        let outcome = quorum.quote("BTC", "USD").await;
        assert!(outcome.price.is_none());
        assert!(outcome
            .events
            .iter()
            .any(|e| matches!(e, OracleEvent::Failover { reason: DegradedReason::Stale, .. })));
    }

    #[tokio::test]
    async fn feed_inside_configured_threshold_is_accepted() {
        let config = QuorumConfig::with_max_staleness_secs(60);
        let quorum = OracleQuorum::new(
            vec![
                source("primary", "100", 30),
                source("secondary", "100", 30),
            ],
            config,
        );
        let outcome = quorum.quote("BTC", "USD").await;
        assert_eq!(outcome.price, Some(BigDecimal::from_str("100").unwrap()));
        assert!(outcome.degraded.is_none());
    }
}
