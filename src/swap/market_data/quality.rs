//! Price feed anomaly detection and per-provider health gating.
//!
//! Evaluation is a pure function over a bounded price window. The result feeds
//! metrics/alerts (never blocking the ingestion hot path - delivery is
//! fire-and-forget) and a persisted `price_provider_health` record that the
//! quoting path consults via [`ProviderHealth::usable_for_quoting`].
//!
//! Threshold policy (per asset pair, see `QualityThresholds`):
//! * staleness: tick older than `max_staleness_secs`.
//! * deviation: |price - trailing median| / median > `max_deviation_bps`. Using
//!   a band around the trailing median (not an absolute level) avoids flapping
//!   in legitimately volatile periods. Skipped on cold start (< `min_history`).
//! * crossed/inverted: bid > ask. zero/negative: any price <= 0 or non-finite.
//! * provider disagreement: spread between providers > `max_provider_spread_bps`.
//!
//! Recovery: a quarantined provider re-enters rotation only after
//! `recovery_window` consecutive clean evaluations. Manual overrides record
//! actor and reason for audit.

use chrono::{DateTime, Utc};
use serde::Serialize;

#[derive(Debug, Clone)]
pub struct QualityThresholds {
    pub max_staleness_secs: i64,
    pub max_deviation_bps: f64,
    pub max_provider_spread_bps: f64,
    pub min_history: usize,
    pub recovery_window: u32,
}

impl Default for QualityThresholds {
    fn default() -> Self {
        Self {
            max_staleness_secs: 30,
            max_deviation_bps: 500.0,
            max_provider_spread_bps: 200.0,
            min_history: 5,
            recovery_window: 10,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tick {
    pub bid: f64,
    pub ask: f64,
    pub at: DateTime<Utc>,
}

impl Tick {
    pub fn mid(&self) -> f64 {
        (self.bid + self.ask) / 2.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Anomaly {
    Stale,
    SuddenDeviation,
    CrossedPrices,
    NonPositive,
    ProviderDisagreement,
}

impl Anomaly {
    /// Label for the metric / alert name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Anomaly::Stale => "stale",
            Anomaly::SuddenDeviation => "sudden_deviation",
            Anomaly::CrossedPrices => "crossed_prices",
            Anomaly::NonPositive => "non_positive",
            Anomaly::ProviderDisagreement => "provider_disagreement",
        }
    }
}

fn median(vals: &[f64]) -> f64 {
    let mut v = vals.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// Evaluate the latest `tick` against the trailing `window` (excluding `tick`)
/// and optionally the mid of another provider.
pub fn evaluate(
    tick: &Tick,
    window: &[Tick],
    other_provider_mid: Option<f64>,
    now: DateTime<Utc>,
    t: &QualityThresholds,
) -> Vec<Anomaly> {
    let mut out = Vec::new();
    if !tick.bid.is_finite() || !tick.ask.is_finite() || tick.bid <= 0.0 || tick.ask <= 0.0 {
        // Remaining checks are meaningless on non-positive data.
        out.push(Anomaly::NonPositive);
        if (now - tick.at).num_seconds() > t.max_staleness_secs {
            out.push(Anomaly::Stale);
        }
        return out;
    }
    if (now - tick.at).num_seconds() > t.max_staleness_secs {
        out.push(Anomaly::Stale);
    }
    if tick.bid > tick.ask {
        out.push(Anomaly::CrossedPrices);
    }
    if window.len() >= t.min_history {
        let med = median(&window.iter().map(Tick::mid).collect::<Vec<_>>());
        if med > 0.0 && ((tick.mid() - med).abs() / med) * 10_000.0 > t.max_deviation_bps {
            out.push(Anomaly::SuddenDeviation);
        }
    }
    if let Some(o) = other_provider_mid.filter(|o| *o > 0.0) {
        if ((tick.mid() - o).abs() / o) * 10_000.0 > t.max_provider_spread_bps {
            out.push(Anomaly::ProviderDisagreement);
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    Healthy,
    Quarantined,
}

/// Persisted per pair/provider health record.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProviderHealth {
    pub state: HealthState,
    pub clean_streak: u32,
    pub last_anomalies: Vec<Anomaly>,
    /// Manual override: `Some(actor, reason)` forces the state until cleared.
    pub override_by: Option<(String, String)>,
}

impl Default for ProviderHealth {
    fn default() -> Self {
        Self {
            state: HealthState::Healthy,
            clean_streak: 0,
            last_anomalies: vec![],
            override_by: None,
        }
    }
}

impl ProviderHealth {
    pub fn usable_for_quoting(&self) -> bool {
        self.state == HealthState::Healthy
    }

    /// Fold one evaluation result into the health record.
    pub fn observe(&mut self, anomalies: &[Anomaly], t: &QualityThresholds) {
        if self.override_by.is_some() {
            return;
        }
        if !anomalies.is_empty() {
            self.state = HealthState::Quarantined;
            self.clean_streak = 0;
            self.last_anomalies = anomalies.to_vec();
        } else if self.state == HealthState::Quarantined {
            self.clean_streak += 1;
            if self.clean_streak >= t.recovery_window {
                self.state = HealthState::Healthy;
                self.last_anomalies.clear();
            }
        }
    }

    /// Auditable manual override; the caller persists actor + reason.
    pub fn set_override(&mut self, state: HealthState, actor: &str, reason: &str) {
        self.state = state;
        self.clean_streak = 0;
        self.override_by = Some((actor.to_string(), reason.to_string()));
    }

    pub fn clear_override(&mut self) {
        self.override_by = None;
    }
}

/// Providers whose prices may be used for quoting.
pub fn usable_providers<'a>(health: &'a [(&'a str, ProviderHealth)]) -> Vec<&'a str> {
    health
        .iter()
        .filter(|(_, h)| h.usable_for_quoting())
        .map(|(n, _)| *n)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn tick(bid: f64, ask: f64, age: i64, now: DateTime<Utc>) -> Tick {
        Tick {
            bid,
            ask,
            at: now - Duration::seconds(age),
        }
    }

    fn window(now: DateTime<Utc>) -> Vec<Tick> {
        (0..6).map(|i| tick(0.999, 1.001, 60 - i, now)).collect()
    }

    #[test]
    fn clean_tick() {
        let now = Utc::now();
        let t = QualityThresholds::default();
        assert!(evaluate(
            &tick(0.999, 1.001, 1, now),
            &window(now),
            Some(1.0),
            now,
            &t
        )
        .is_empty());
    }

    #[test]
    fn each_anomaly_class() {
        let now = Utc::now();
        let t = QualityThresholds::default();
        let w = window(now);
        assert_eq!(
            evaluate(&tick(1.0, 1.0, 120, now), &w, None, now, &t),
            vec![Anomaly::Stale]
        );
        assert_eq!(
            evaluate(&tick(1.2, 1.4, 1, now), &w, None, now, &t),
            vec![Anomaly::SuddenDeviation]
        );
        assert!(
            evaluate(&tick(1.01, 1.0, 1, now), &w, None, now, &t).contains(&Anomaly::CrossedPrices)
        );
        assert_eq!(
            evaluate(&tick(0.0, 1.0, 1, now), &w, None, now, &t),
            vec![Anomaly::NonPositive]
        );
        assert_eq!(
            evaluate(&tick(-1.0, 1.0, 1, now), &w, None, now, &t),
            vec![Anomaly::NonPositive]
        );
        assert_eq!(
            evaluate(&tick(0.999, 1.001, 1, now), &w, Some(1.05), now, &t),
            vec![Anomaly::ProviderDisagreement]
        );
    }

    #[test]
    fn cold_start_skips_deviation() {
        let now = Utc::now();
        let t = QualityThresholds::default();
        assert!(evaluate(&tick(5.0, 5.0, 1, now), &[], None, now, &t).is_empty());
    }

    #[test]
    fn volatility_within_band_not_flagged() {
        let now = Utc::now();
        let t = QualityThresholds::default();
        assert!(evaluate(&tick(1.03, 1.05, 1, now), &window(now), None, now, &t).is_empty());
    }

    #[test]
    fn quarantine_and_recovery() {
        let t = QualityThresholds {
            recovery_window: 3,
            ..Default::default()
        };
        let mut h = ProviderHealth::default();
        h.observe(&[Anomaly::Stale], &t);
        assert!(!h.usable_for_quoting());
        h.observe(&[], &t);
        h.observe(&[], &t);
        assert!(!h.usable_for_quoting());
        h.observe(&[], &t);
        assert!(h.usable_for_quoting());
    }

    #[test]
    fn anomaly_resets_recovery_streak() {
        let t = QualityThresholds {
            recovery_window: 2,
            ..Default::default()
        };
        let mut h = ProviderHealth::default();
        h.observe(&[Anomaly::Stale], &t);
        h.observe(&[], &t);
        h.observe(&[Anomaly::Stale], &t);
        h.observe(&[], &t);
        assert!(!h.usable_for_quoting());
    }

    #[test]
    fn override_is_audited_and_sticky() {
        let t = QualityThresholds::default();
        let mut h = ProviderHealth::default();
        h.set_override(HealthState::Quarantined, "ops", "vendor incident");
        h.observe(&[], &t);
        assert_eq!(
            h.override_by,
            Some(("ops".into(), "vendor incident".into()))
        );
        assert!(!h.usable_for_quoting());
        h.clear_override();
    }

    #[test]
    fn quoting_excludes_quarantined_then_resumes() {
        let t = QualityThresholds {
            recovery_window: 1,
            ..Default::default()
        };
        let mut bad = ProviderHealth::default();
        bad.observe(&[Anomaly::SuddenDeviation], &t);
        let mut set = vec![("a", ProviderHealth::default()), ("b", bad)];
        assert_eq!(usable_providers(&set), vec!["a"]);
        set[1].1.observe(&[], &t);
        assert_eq!(usable_providers(&set), vec!["a", "b"]);
    }
}
