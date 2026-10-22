//! Liquidity provider health scoring and failover routing (#1530).
//!
//! Deterministic: callers supply timestamps, so scoring/hysteresis are unit-testable
//! and immune to wall-clock skew inside the module.

use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone)]
pub struct HealthConfig {
    pub window_secs: i64,
    /// Provider becomes unhealthy below this score.
    pub unhealthy_below: f64,
    /// ...and only recovers at or above this (hysteresis, > unhealthy_below).
    pub recover_at: f64,
    pub max_latency_ms: f64,
    pub max_quote_age_ms: f64,
    /// Below this many samples the provider is treated as healthy (sparse data).
    pub min_samples: usize,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            window_secs: 300,
            unhealthy_below: 0.5,
            recover_at: 0.7,
            max_latency_ms: 2000.0,
            max_quote_age_ms: 5000.0,
            min_samples: 5,
        }
    }
}

impl HealthConfig {
    /// Thresholds from `LP_HEALTH_UNHEALTHY_BELOW` / `LP_HEALTH_RECOVER_AT` / `LP_HEALTH_WINDOW_SECS`.
    pub fn from_env() -> Self {
        let mut c = Self::default();
        let f = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<f64>().ok());
        if let Some(v) = f("LP_HEALTH_UNHEALTHY_BELOW") {
            c.unhealthy_below = v;
        }
        if let Some(v) = f("LP_HEALTH_RECOVER_AT") {
            c.recover_at = v.max(c.unhealthy_below);
        }
        if let Some(v) = f("LP_HEALTH_WINDOW_SECS") {
            c.window_secs = v as i64;
        }
        c
    }
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    at: i64,
    latency_ms: f64,
    ok: bool,
    quote_age_ms: f64,
    /// Realised/quoted price ratio, clamped to [0,1] (1 = perfect fill).
    fill_quality: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HealthEvent {
    BecameUnhealthy {
        provider: String,
        score: f64,
    },
    Recovered {
        provider: String,
        score: f64,
    },
    Failover {
        pair: String,
        from: String,
        to: String,
    },
}

#[derive(Debug, Default)]
struct ProviderState {
    samples: VecDeque<Sample>,
    healthy: bool,
}

#[derive(Debug, Default)]
pub struct HealthTracker {
    cfg: HealthConfig,
    providers: HashMap<String, ProviderState>,
}

impl HealthTracker {
    pub fn new(cfg: HealthConfig) -> Self {
        Self {
            cfg,
            providers: HashMap::new(),
        }
    }

    /// Record an observation; returns any health transition event.
    pub fn record(
        &mut self,
        provider: &str,
        now: i64,
        latency_ms: f64,
        ok: bool,
        quote_age_ms: f64,
        fill_quality: f64,
    ) -> Option<HealthEvent> {
        let window = self.cfg.window_secs;
        let st = self
            .providers
            .entry(provider.to_string())
            .or_insert_with(|| ProviderState {
                samples: VecDeque::new(),
                healthy: true,
            });
        st.samples.push_back(Sample {
            at: now,
            latency_ms,
            ok,
            quote_age_ms,
            fill_quality: fill_quality.clamp(0.0, 1.0),
        });
        while st.samples.front().is_some_and(|s| now - s.at > window) {
            st.samples.pop_front();
        }
        let score = Self::score_of(&self.cfg, &st.samples);
        let event = if st.healthy && score < self.cfg.unhealthy_below {
            st.healthy = false;
            Some(HealthEvent::BecameUnhealthy {
                provider: provider.into(),
                score,
            })
        } else if !st.healthy && score >= self.cfg.recover_at {
            st.healthy = true;
            Some(HealthEvent::Recovered {
                provider: provider.into(),
                score,
            })
        } else {
            None
        };
        if let Some(e) = &event {
            tracing::warn!(event = ?e, "lp_health_transition");
        }
        event
    }

    fn score_of(cfg: &HealthConfig, s: &VecDeque<Sample>) -> f64 {
        if s.len() < cfg.min_samples {
            return 1.0;
        }
        let n = s.len() as f64;
        let ok_rate = s.iter().filter(|x| x.ok).count() as f64 / n;
        let lat = s
            .iter()
            .map(|x| 1.0 - (x.latency_ms / cfg.max_latency_ms).min(1.0))
            .sum::<f64>()
            / n;
        let fresh = s
            .iter()
            .map(|x| 1.0 - (x.quote_age_ms / cfg.max_quote_age_ms).min(1.0))
            .sum::<f64>()
            / n;
        let fill = s.iter().map(|x| x.fill_quality).sum::<f64>() / n;
        0.4 * ok_rate + 0.2 * lat + 0.2 * fresh + 0.2 * fill
    }

    pub fn score(&self, provider: &str) -> f64 {
        self.providers
            .get(provider)
            .map_or(1.0, |p| Self::score_of(&self.cfg, &p.samples))
    }

    pub fn is_healthy(&self, provider: &str) -> bool {
        self.providers.get(provider).map_or(true, |p| p.healthy)
    }
}

/// Manual operator controls; these always take precedence over health.
#[derive(Debug, Default, Clone)]
pub struct Overrides {
    /// Providers forced eligible regardless of health.
    pub force_enabled: HashSet<String>,
    /// Providers disabled globally.
    pub force_disabled: HashSet<String>,
    /// (provider, pair) exclusions.
    pub excluded_pairs: HashSet<(String, String)>,
}

pub struct Router {
    pub tracker: HealthTracker,
    pub overrides: Overrides,
    /// Last provider chosen per pair, to report failovers.
    last: HashMap<String, String>,
}

impl Router {
    pub fn new(tracker: HealthTracker, overrides: Overrides) -> Self {
        Self {
            tracker,
            overrides,
            last: HashMap::new(),
        }
    }

    fn eligible(&self, provider: &str, pair: &str) -> bool {
        let o = &self.overrides;
        if o.force_disabled.contains(provider)
            || o.excluded_pairs
                .contains(&(provider.to_string(), pair.to_string()))
        {
            return false;
        }
        o.force_enabled.contains(provider) || self.tracker.is_healthy(provider)
    }

    /// Eligible providers for `pair`, best score first (ties broken by name for determinism).
    pub fn candidates<'a>(&self, pair: &str, providers: &'a [String]) -> Vec<&'a String> {
        let mut v: Vec<_> = providers
            .iter()
            .filter(|p| self.eligible(p, pair))
            .collect();
        v.sort_by(|a, b| {
            self.tracker
                .score(b)
                .partial_cmp(&self.tracker.score(a))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.cmp(b))
        });
        v
    }

    /// Pick the provider for `pair`; the second value is a failover event if it changed.
    pub fn select(
        &mut self,
        pair: &str,
        providers: &[String],
    ) -> Option<(String, Option<HealthEvent>)> {
        let chosen = self
            .candidates(pair, providers)
            .first()
            .map(|s| (*s).clone())?;
        let ev = match self.last.insert(pair.to_string(), chosen.clone()) {
            Some(prev) if prev != chosen => {
                let e = HealthEvent::Failover {
                    pair: pair.into(),
                    from: prev,
                    to: chosen.clone(),
                };
                tracing::warn!(event = ?e, "lp_failover");
                Some(e)
            }
            _ => None,
        };
        Some((chosen, ev))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker() -> HealthTracker {
        HealthTracker::new(HealthConfig::default())
    }

    fn feed(t: &mut HealthTracker, p: &str, start: i64, n: i64, ok: bool) -> Vec<HealthEvent> {
        (0..n)
            .filter_map(|i| {
                if ok {
                    t.record(p, start + i, 100.0, true, 100.0, 1.0)
                } else {
                    t.record(p, start + i, 5000.0, false, 9000.0, 0.0)
                }
            })
            .collect()
    }

    #[test]
    fn sparse_data_stays_healthy() {
        let mut t = tracker();
        assert!(feed(&mut t, "a", 0, 4, false).is_empty());
        assert!(t.is_healthy("a"));
    }

    #[test]
    fn degrades_then_recovers_with_hysteresis() {
        let mut t = tracker();
        let ev = feed(&mut t, "a", 0, 10, false);
        assert!(matches!(ev[0], HealthEvent::BecameUnhealthy { .. }));
        assert!(!t.is_healthy("a"));
        // old failures age out of the window; good samples bring score back up
        let ev = feed(&mut t, "a", 1000, 10, true);
        assert!(ev
            .iter()
            .any(|e| matches!(e, HealthEvent::Recovered { .. })));
        assert!(t.is_healthy("a"));
    }

    #[test]
    fn failover_routes_away_and_reports() {
        let providers = vec!["a".to_string(), "b".to_string()];
        let mut r = Router::new(tracker(), Overrides::default());
        assert_eq!(r.select("XLM/USDC", &providers).unwrap().0, "a");
        feed(&mut r.tracker, "a", 0, 10, false);
        let (p, ev) = r.select("XLM/USDC", &providers).unwrap();
        assert_eq!(p, "b");
        assert!(matches!(ev, Some(HealthEvent::Failover { .. })));
    }

    #[test]
    fn overrides_take_precedence() {
        let providers = vec!["a".to_string(), "b".to_string()];
        let mut r = Router::new(tracker(), Overrides::default());
        feed(&mut r.tracker, "a", 0, 10, false);
        r.overrides.force_enabled.insert("a".into());
        assert!(r.candidates("P", &providers).iter().any(|p| *p == "a"));
        r.overrides.force_disabled.insert("a".into());
        assert!(!r.candidates("P", &providers).iter().any(|p| *p == "a"));
        r.overrides.excluded_pairs.insert(("b".into(), "P".into()));
        assert!(r.select("P", &providers).is_none());
        assert!(r.select("Q", &providers).is_some());
    }
}
