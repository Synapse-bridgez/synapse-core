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
            max_staleness: Duration::seconds(30),
            degraded_spread_widen_bps: 100,
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
            events.push(OracleEvent::Divergence {
                spread_bps: self.spread_bps(&fresh),
                sources: fresh.iter().map(|o| o.source.clone()).collect(),
            });
        }

        let mode = match reason {
            DegradedReason::Divergent => DegradedMode::Halt,
            DegradedReason::Stale | DegradedReason::Unavailable => {
                if fresh.is_empty() {
                    DegradedMode::Halt
                } else {
                    DegradedMode::WidenSpread {
                        widen_bps: self.config.degraded_spread_widen_bps,
                    }
                }
            }
        };

        let (price, sources) = match mode {
            DegradedMode::Halt => (None, Vec::new()),
            _ => {
                let best = fresh
                    .iter()
                    .max_by(|a, b| a.observed_at.cmp(&b.observed_at))
                    .expect("non-empty in non-halt mode");
                (Some(best.price.clone()), vec![best.source.clone()])
            }
        };

        events.push(OracleEvent::Failover {
            reason,
            mode,
            sources: sources.clone(),
        });

        QuorumOutcome {
            price,
            sources,
            degraded: Some(mode),
            events,
        }
    }

    /// Largest subset of `observations` that agree within tolerance.
    fn agreeing_group(&self, observations: &[PriceObservation]) -> Option<Vec<PriceObservation>> {
        let mut best: Vec<PriceObservation> = Vec::new();
        for anchor in observations {
            let group: Vec<PriceObservation> = observations
                .iter()
                .filter(|o| within_tolerance(&anchor.price, &o.price, self.config.tolerance_bps))
                .cloned()
                .collect();
            if group.len() > best.len() {
                best = group;
            }
        }
        if best.len() >= self.config.min_agreeing_sources {
            Some(best)
        } else {
            None
        }
    }

    /// Max relative deviation across `observations`, in basis points.
    fn spread_bps(&self, observations: &[PriceObservation]) -> u32 {
        let mut min: Option<&BigDecimal> = None;
        let mut max: Option<&BigDecimal> = None;
        for obs in observations {
            if min.map_or(true, |m| obs.price < *m) {
                min = Some(&obs.price);
            }
            if max.map_or(true, |m| obs.price > *m) {
                max = Some(&obs.price);
            }
        }
        match (min, max) {
            (Some(min), Some(max)) if *min > BigDecimal::from(0) => {
                let diff = max - min;
                let bps = (diff / min) * BigDecimal::from(10_000);
                bps.to_string().parse::<f64>().unwrap_or(0.0).round() as u32
            }
            _ => 0,
        }
    }
}

/// Whether `a` and `b` are within `tolerance_bps` of each other.
fn within_tolerance(a: &BigDecimal, b: &BigDecimal, tolerance_bps: u32) -> bool {
    if *a <= BigDecimal::from(0) || *b <= BigDecimal::from(0) {
        return false;
    }
    let diff = if a > b { a - b } else { b - a };
    let base = if a < b { a } else { b };
    let ratio = (diff / base) * BigDecimal::from(10_000);
    ratio <= BigDecimal::from(tolerance_bps)
}

/// Median price of a non-empty group.
fn median(group: &[PriceObservation]) -> BigDecimal {
    let mut prices: Vec<&BigDecimal> = group.iter().map(|o| &o.price).collect();
    prices.sort();
    prices[prices.len() / 2].clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(source: &str, price: i64) -> PriceObservation {
        PriceObservation {
            source: source.into(),
            price: BigDecimal::from(price),
            observed_at: Utc::now(),
        }
    }

    #[test]
    fn tolerance_accepts_close_prices() {
        assert!(within_tolerance(
            &BigDecimal::from(100),
            &BigDecimal::from(100),
            50
        ));
        assert!(!within_tolerance(
            &BigDecimal::from(100),
            &BigDecimal::from(120),
            50
        ));
    }

    #[test]
    fn median_picks_middle_price() {
        let group = vec![obs("a", 100), obs("b", 101), obs("c", 102)];
        assert_eq!(median(&group), BigDecimal::from(101));
    }

    #[tokio::test]
    async fn quorum_met_when_sources_agree() {
        let quorum = OracleQuorum::new(
            vec![
                Arc::new(StaticPriceSource::new("primary", BigDecimal::from(100))),
                Arc::new(StaticPriceSource::new("secondary", BigDecimal::from(101))),
            ],
            QuorumConfig::default(),
        );
        let outcome = quorum.quote("XLM", "USDC").await;
        assert_eq!(outcome.price, Some(BigDecimal::from(101)));
        assert!(outcome.degraded.is_none());
        assert_eq!(outcome.sources.len(), 2);
    }

    #[tokio::test]
    async fn divergence_halts_quoting() {
        let quorum = OracleQuorum::new(
            vec![
                Arc::new(StaticPriceSource::new("primary", BigDecimal::from(100))),
                Arc::new(StaticPriceSource::new("secondary", BigDecimal::from(200))),
            ],
            QuorumConfig::default(),
        );
        let outcome = quorum.quote("XLM", "USDC").await;
        assert_eq!(outcome.price, None);
        assert_eq!(outcome.degraded, Some(DegradedMode::Halt));
        assert!(outcome
            .events
            .iter()
            .any(|e| matches!(e, OracleEvent::Divergence { .. })));
    }

    #[tokio::test]
    async fn single_source_widens_spread() {
        let quorum = OracleQuorum::new(
            vec![Arc::new(StaticPriceSource::new(
                "primary",
                BigDecimal::from(100),
            ))],
            QuorumConfig::default(),
        );
        let outcome = quorum.quote("XLM", "USDC").await;
        assert_eq!(outcome.price, Some(BigDecimal::from(100)));
        assert!(matches!(
            outcome.degraded,
            Some(DegradedMode::WidenSpread { .. })
        ));
    }
}
