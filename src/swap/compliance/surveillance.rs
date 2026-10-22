//! Rules-based trade surveillance over recent swap activity (#1505).
//!
//! Rules are small evaluators over a shared [`Window`] + activity slice, so a
//! new rule is one `impl Rule` plus an entry in [`default_rules`]; the runner
//! never changes. Everything here is read-only against `swap_activity` and runs
//! on a background schedule, so it can never delay order execution.
//!
//! Findings are idempotent per `(rule, scope, window_start)` (`dedupe_key`), so
//! re-running a window never duplicates rows or alerts. See
//! `docs/swap-platform-hardening.md` for the rule catalogue and thresholds.

use crate::alerting::{names, AlertPayload, AlertSeverity};
use bigdecimal::{BigDecimal, ToPrimitive};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

/// One row of `swap_activity`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SwapActivity {
    pub tenant_id: Uuid,
    pub account: String,
    pub pair: String,
    pub route: String,
    pub direction: String,
    pub kind: String,
    pub status: String,
    pub amount: BigDecimal,
    pub price: Option<BigDecimal>,
    pub ref_price: Option<BigDecimal>,
    pub counterparty_tenant_id: Option<Uuid>,
    pub owner_group: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Half-open evaluation window `[start, end)`.
#[derive(Debug, Clone, Copy)]
pub struct Window {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl Window {
    pub fn len(&self) -> Duration {
        self.end - self.start
    }

    fn contains(&self, t: DateTime<Utc>) -> bool {
        t >= self.start && t < self.end
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Low => "low",
            Severity::Medium => "medium",
            Severity::High => "high",
            Severity::Critical => "critical",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub rule: &'static str,
    pub severity: Severity,
    pub tenant_ids: Vec<Uuid>,
    pub accounts: Vec<String>,
    pub pair: Option<String>,
    pub route: Option<String>,
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    pub evidence: serde_json::Value,
    /// Stable per (rule, scope, window): makes persistence idempotent.
    pub dedupe_key: String,
}

/// Rule thresholds; every field is overridable via `SWAP_SURVEILLANCE_*` env.
#[derive(Debug, Clone)]
pub struct SurveillanceConfig {
    pub window_secs: i64,
    pub self_dealing_min_trades: usize,
    pub wash_min_trades: usize,
    /// Max |buy - sell| / max(buy, sell) for a flow to count as circular.
    pub wash_net_tolerance: f64,
    pub probing_min_quotes: usize,
    /// Quote probing fires when executed/quoted is below this ratio.
    pub probing_max_exec_ratio: f64,
    pub spike_multiplier: f64,
    pub spike_min_volume: f64,
    /// Average price advantage vs reference (bps) that counts as favorable.
    pub spike_favorable_bps: f64,
    pub baseline_windows: i64,
}

impl Default for SurveillanceConfig {
    fn default() -> Self {
        Self {
            window_secs: 3600,
            self_dealing_min_trades: 3,
            wash_min_trades: 4,
            wash_net_tolerance: 0.05,
            probing_min_quotes: 50,
            probing_max_exec_ratio: 0.02,
            spike_multiplier: 5.0,
            spike_min_volume: 10_000.0,
            spike_favorable_bps: 25.0,
            baseline_windows: 24,
        }
    }
}

impl SurveillanceConfig {
    pub fn from_env() -> Self {
        fn env<T: std::str::FromStr>(key: &str, default: T) -> T {
            std::env::var(key)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(default)
        }
        let d = Self::default();
        Self {
            window_secs: env("SWAP_SURVEILLANCE_WINDOW_SECS", d.window_secs),
            self_dealing_min_trades: env(
                "SWAP_SURVEILLANCE_SELF_DEALING_MIN_TRADES",
                d.self_dealing_min_trades,
            ),
            wash_min_trades: env("SWAP_SURVEILLANCE_WASH_MIN_TRADES", d.wash_min_trades),
            wash_net_tolerance: env("SWAP_SURVEILLANCE_WASH_NET_TOLERANCE", d.wash_net_tolerance),
            probing_min_quotes: env("SWAP_SURVEILLANCE_PROBING_MIN_QUOTES", d.probing_min_quotes),
            probing_max_exec_ratio: env(
                "SWAP_SURVEILLANCE_PROBING_MAX_EXEC_RATIO",
                d.probing_max_exec_ratio,
            ),
            spike_multiplier: env("SWAP_SURVEILLANCE_SPIKE_MULTIPLIER", d.spike_multiplier),
            spike_min_volume: env("SWAP_SURVEILLANCE_SPIKE_MIN_VOLUME", d.spike_min_volume),
            spike_favorable_bps: env(
                "SWAP_SURVEILLANCE_SPIKE_FAVORABLE_BPS",
                d.spike_favorable_bps,
            ),
            baseline_windows: env("SWAP_SURVEILLANCE_BASELINE_WINDOWS", d.baseline_windows),
        }
    }
}

/// A surveillance rule. `activity` covers at least `window` (spike detection
/// also receives the preceding baseline period) and must not be mutated.
pub trait Rule: Send + Sync {
    fn name(&self) -> &'static str;
    fn evaluate(&self, window: &Window, activity: &[SwapActivity]) -> Vec<Finding>;
}

fn f(v: &BigDecimal) -> f64 {
    v.to_f64().unwrap_or(0.0)
}

fn orders_in<'a>(
    window: &'a Window,
    activity: &'a [SwapActivity],
) -> impl Iterator<Item = &'a SwapActivity> {
    activity
        .iter()
        .filter(move |a| a.kind == "order" && a.status == "filled" && window.contains(a.created_at))
}

fn finding(
    rule: &'static str,
    severity: Severity,
    window: &Window,
    scope: &str,
    tenant_ids: BTreeSet<Uuid>,
    accounts: BTreeSet<String>,
    pair: Option<&str>,
    route: Option<&str>,
    evidence: serde_json::Value,
) -> Finding {
    Finding {
        rule,
        severity,
        tenant_ids: tenant_ids.into_iter().collect(),
        accounts: accounts.into_iter().collect(),
        pair: pair.map(str::to_string),
        route: route.map(str::to_string),
        window_start: window.start,
        window_end: window.end,
        evidence,
        dedupe_key: format!("{rule}:{scope}:{}", window.start.timestamp()),
    }
}

/// Fills between different tenants that share an `owner_group`.
pub struct SelfDealing {
    pub min_trades: usize,
}

impl Rule for SelfDealing {
    fn name(&self) -> &'static str {
        "self_dealing"
    }

    fn evaluate(&self, window: &Window, activity: &[SwapActivity]) -> Vec<Finding> {
        let mut groups: BTreeMap<(String, String), Vec<&SwapActivity>> = BTreeMap::new();
        for a in orders_in(window, activity) {
            let (Some(cp), Some(g)) = (a.counterparty_tenant_id, a.owner_group.as_ref()) else {
                continue;
            };
            if cp != a.tenant_id {
                groups
                    .entry((g.clone(), a.pair.clone()))
                    .or_default()
                    .push(a);
            }
        }
        groups
            .into_iter()
            .filter(|(_, v)| v.len() >= self.min_trades)
            .map(|((group, pair), v)| {
                let mut tenants: BTreeSet<Uuid> = v.iter().map(|a| a.tenant_id).collect();
                tenants.extend(v.iter().filter_map(|a| a.counterparty_tenant_id));
                let accounts = v.iter().map(|a| a.account.clone()).collect();
                let volume: f64 = v.iter().map(|a| f(&a.amount)).sum();
                finding(
                    self.name(),
                    Severity::High,
                    window,
                    &format!("{group}:{pair}"),
                    tenants,
                    accounts,
                    Some(&pair),
                    None,
                    serde_json::json!({ "owner_group": group, "trades": v.len(), "volume": volume }),
                )
            })
            .collect()
    }
}

/// Buy/sell flows per tenant+pair that net to ~zero over many trades.
pub struct WashCircularFlow {
    pub min_trades: usize,
    pub net_tolerance: f64,
}

impl Rule for WashCircularFlow {
    fn name(&self) -> &'static str {
        "wash_circular_flow"
    }

    fn evaluate(&self, window: &Window, activity: &[SwapActivity]) -> Vec<Finding> {
        let mut groups: BTreeMap<(Uuid, String), (f64, f64, usize, BTreeSet<String>)> =
            BTreeMap::new();
        for a in orders_in(window, activity) {
            let e = groups.entry((a.tenant_id, a.pair.clone())).or_default();
            if a.direction == "buy" {
                e.0 += f(&a.amount);
            } else {
                e.1 += f(&a.amount);
            }
            e.2 += 1;
            e.3.insert(a.account.clone());
        }
        groups
            .into_iter()
            .filter_map(|((tenant, pair), (buy, sell, n, accounts))| {
                let big = buy.max(sell);
                if n < self.min_trades
                    || buy <= 0.0
                    || sell <= 0.0
                    || (buy - sell).abs() / big > self.net_tolerance
                {
                    return None;
                }
                Some(finding(
                    self.name(),
                    Severity::Medium,
                    window,
                    &format!("{tenant}:{pair}"),
                    BTreeSet::from([tenant]),
                    accounts,
                    Some(&pair),
                    None,
                    serde_json::json!({ "buy_volume": buy, "sell_volume": sell, "trades": n }),
                ))
            })
            .collect()
    }
}

/// Many quotes with almost no execution (scraping / latency probing).
pub struct QuoteProbing {
    pub min_quotes: usize,
    pub max_exec_ratio: f64,
}

impl Rule for QuoteProbing {
    fn name(&self) -> &'static str {
        "quote_probing"
    }

    fn evaluate(&self, window: &Window, activity: &[SwapActivity]) -> Vec<Finding> {
        let mut groups: BTreeMap<(Uuid, String), (usize, usize, BTreeSet<String>)> =
            BTreeMap::new();
        for a in activity.iter().filter(|a| window.contains(a.created_at)) {
            let e = groups.entry((a.tenant_id, a.pair.clone())).or_default();
            if a.kind == "quote" {
                e.0 += 1;
                e.2.insert(a.account.clone());
            } else if a.status == "filled" {
                e.1 += 1;
            }
        }
        groups
            .into_iter()
            .filter_map(|((tenant, pair), (quotes, fills, accounts))| {
                let ratio = fills as f64 / quotes.max(1) as f64;
                if quotes < self.min_quotes || ratio > self.max_exec_ratio {
                    return None;
                }
                Some(finding(
                    self.name(),
                    Severity::Low,
                    window,
                    &format!("{tenant}:{pair}"),
                    BTreeSet::from([tenant]),
                    accounts,
                    Some(&pair),
                    None,
                    serde_json::json!({ "quotes": quotes, "fills": fills, "exec_ratio": ratio }),
                ))
            })
            .collect()
    }
}

/// Volume far above the trailing baseline, paired with favorable pricing.
pub struct VolumeSpikeFavorablePricing {
    pub multiplier: f64,
    pub min_volume: f64,
    pub favorable_bps: f64,
    pub baseline_windows: i64,
}

impl Rule for VolumeSpikeFavorablePricing {
    fn name(&self) -> &'static str {
        "volume_spike_favorable_pricing"
    }

    fn evaluate(&self, window: &Window, activity: &[SwapActivity]) -> Vec<Finding> {
        let baseline_start = window.start - window.len() * self.baseline_windows as i32;
        let baseline = Window {
            start: baseline_start,
            end: window.start,
        };
        // (current volume, bps-weighted advantage numerator, baseline volume, accounts)
        let mut groups: BTreeMap<(Uuid, String, String), (f64, f64, f64, BTreeSet<String>)> =
            BTreeMap::new();
        for a in orders_in(window, activity) {
            let e = groups
                .entry((a.tenant_id, a.pair.clone(), a.route.clone()))
                .or_default();
            let amt = f(&a.amount);
            e.0 += amt;
            if let (Some(p), Some(r)) = (&a.price, &a.ref_price) {
                let (p, r) = (f(p), f(r));
                if r > 0.0 {
                    // Buyer favors a lower price, seller a higher one.
                    let adv = if a.direction == "buy" {
                        (r - p) / r
                    } else {
                        (p - r) / r
                    };
                    e.1 += adv * 10_000.0 * amt;
                }
            }
            e.3.insert(a.account.clone());
        }
        for a in orders_in(&baseline, activity) {
            if let Some(e) = groups.get_mut(&(a.tenant_id, a.pair.clone(), a.route.clone())) {
                e.2 += f(&a.amount);
            }
        }
        groups
            .into_iter()
            .filter_map(|((tenant, pair, route), (vol, adv_num, base_total, accounts))| {
                let base_avg = base_total / self.baseline_windows.max(1) as f64;
                let avg_adv_bps = if vol > 0.0 { adv_num / vol } else { 0.0 };
                if vol < self.min_volume || vol < base_avg * self.multiplier || avg_adv_bps < self.favorable_bps {
                    return None;
                }
                Some(finding(
                    self.name(),
                    Severity::High,
                    window,
                    &format!("{tenant}:{pair}:{route}"),
                    BTreeSet::from([tenant]),
                    accounts,
                    Some(&pair),
                    Some(&route),
                    serde_json::json!({ "volume": vol, "baseline_avg": base_avg, "avg_advantage_bps": avg_adv_bps }),
                ))
            })
            .collect()
    }
}

pub fn default_rules(cfg: &SurveillanceConfig) -> Vec<Box<dyn Rule>> {
    vec![
        Box::new(SelfDealing {
            min_trades: cfg.self_dealing_min_trades,
        }),
        Box::new(WashCircularFlow {
            min_trades: cfg.wash_min_trades,
            net_tolerance: cfg.wash_net_tolerance,
        }),
        Box::new(QuoteProbing {
            min_quotes: cfg.probing_min_quotes,
            max_exec_ratio: cfg.probing_max_exec_ratio,
        }),
        Box::new(VolumeSpikeFavorablePricing {
            multiplier: cfg.spike_multiplier,
            min_volume: cfg.spike_min_volume,
            favorable_bps: cfg.spike_favorable_bps,
            baseline_windows: cfg.baseline_windows,
        }),
    ]
}

/// Runs every rule over `window`; pure, so it is trivially testable.
pub fn evaluate_rules(
    rules: &[Box<dyn Rule>],
    window: &Window,
    activity: &[SwapActivity],
) -> Vec<Finding> {
    rules
        .iter()
        .flat_map(|r| r.evaluate(window, activity))
        .collect()
}

/// Persists findings idempotently; returns the findings newly inserted, and
/// raises an alert for each new high/critical one.
pub async fn persist_findings(
    pool: &PgPool,
    findings: Vec<Finding>,
) -> Result<Vec<Finding>, sqlx::Error> {
    let mut inserted = Vec::new();
    for fnd in findings {
        let res = sqlx::query(
            "INSERT INTO surveillance_findings
               (dedupe_key, rule, severity, tenant_ids, accounts, pair, route, window_start, window_end, evidence)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
             ON CONFLICT (dedupe_key) DO NOTHING",
        )
        .bind(&fnd.dedupe_key)
        .bind(fnd.rule)
        .bind(fnd.severity.as_str())
        .bind(&fnd.tenant_ids)
        .bind(&fnd.accounts)
        .bind(&fnd.pair)
        .bind(&fnd.route)
        .bind(fnd.window_start)
        .bind(fnd.window_end)
        .bind(&fnd.evidence)
        .execute(pool)
        .await?;
        if res.rows_affected() == 1 {
            if fnd.severity >= Severity::High {
                AlertPayload::new(
                    names::SWAP_SURVEILLANCE_HIGH_SEVERITY,
                    if fnd.severity == Severity::Critical {
                        AlertSeverity::Critical
                    } else {
                        AlertSeverity::Warning
                    },
                    format!(
                        "swap surveillance: {} finding for {} tenant(s)",
                        fnd.rule,
                        fnd.tenant_ids.len()
                    ),
                )
                .with_label("rule", fnd.rule)
                .with_label("severity", fnd.severity.as_str())
                .with_label("dedupe_key", &fnd.dedupe_key)
                .emit();
            }
            inserted.push(fnd);
        }
    }
    Ok(inserted)
}

/// Evaluates the window ending at `now` (aligned to the window length so
/// repeated runs in the same window yield the same `dedupe_key`s).
pub async fn run_once(
    pool: &PgPool,
    cfg: &SurveillanceConfig,
    now: DateTime<Utc>,
) -> Result<usize, sqlx::Error> {
    let secs = cfg.window_secs.max(60);
    let end = DateTime::from_timestamp(now.timestamp().div_euclid(secs) * secs, 0).unwrap_or(now);
    let window = Window {
        start: end - Duration::seconds(secs),
        end,
    };
    let from = window.start - Duration::seconds(secs * cfg.baseline_windows);
    let activity: Vec<SwapActivity> = sqlx::query_as(
        "SELECT tenant_id, account, pair, route, direction, kind, status, amount, price, ref_price,
                counterparty_tenant_id, owner_group, created_at
         FROM swap_activity WHERE created_at >= $1 AND created_at < $2",
    )
    .bind(from)
    .bind(window.end)
    .fetch_all(pool)
    .await?;
    let findings = evaluate_rules(&default_rules(cfg), &window, &activity);
    Ok(persist_findings(pool, findings).await?.len())
}

/// Deletes findings past `retention_until` (cascades to their audit rows).
pub async fn purge_expired(pool: &PgPool) -> Result<u64, sqlx::Error> {
    Ok(
        sqlx::query("DELETE FROM surveillance_findings WHERE retention_until < now()")
            .execute(pool)
            .await?
            .rows_affected(),
    )
}

/// Background scheduler; opt-in via `SWAP_SURVEILLANCE_ENABLED=true`.
pub fn spawn(pool: PgPool) {
    if std::env::var("SWAP_SURVEILLANCE_ENABLED").ok().as_deref() != Some("true") {
        return;
    }
    tokio::spawn(async move {
        let cfg = SurveillanceConfig::from_env();
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(
            (cfg.window_secs.max(60) / 4) as u64,
        ));
        loop {
            tick.tick().await;
            if let Err(e) = run_once(&pool, &cfg, Utc::now()).await {
                tracing::error!(error = %e, "swap surveillance run failed");
            }
            if let Err(e) = purge_expired(&pool).await {
                tracing::error!(error = %e, "swap surveillance retention purge failed");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap()
    }

    fn window() -> Window {
        Window {
            start: t0(),
            end: t0() + Duration::hours(1),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn act(tenant: Uuid, kind: &str, status: &str, dir: &str, amt: i64, at: i64) -> SwapActivity {
        SwapActivity {
            tenant_id: tenant,
            account: format!("acct-{tenant}"),
            pair: "USDC/XLM".into(),
            route: "default".into(),
            direction: dir.into(),
            kind: kind.into(),
            status: status.into(),
            amount: BigDecimal::from(amt),
            price: None,
            ref_price: None,
            counterparty_tenant_id: None,
            owner_group: None,
            created_at: t0() + Duration::seconds(at),
        }
    }

    #[test]
    fn self_dealing_flags_related_tenants_and_ignores_unrelated() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let mk = |group: &str| -> Vec<SwapActivity> {
            (0..3)
                .map(|i| {
                    let mut x = act(a, "order", "filled", "buy", 100, 10 + i);
                    x.counterparty_tenant_id = Some(b);
                    x.owner_group = Some(group.into());
                    x
                })
                .collect()
        };
        let rule = SelfDealing { min_trades: 3 };
        let hits = rule.evaluate(&window(), &mk("grp"));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].tenant_ids.len(), 2);
        // near miss: only two trades
        assert!(rule.evaluate(&window(), &mk("grp")[..2]).is_empty());
        // near miss: no shared owner group
        let mut none = mk("grp");
        none.iter_mut().for_each(|x| x.owner_group = None);
        assert!(rule.evaluate(&window(), &none).is_empty());
    }

    #[test]
    fn wash_flow_requires_balanced_buy_sell() {
        let t = Uuid::new_v4();
        let rule = WashCircularFlow {
            min_trades: 4,
            net_tolerance: 0.05,
        };
        let balanced = vec![
            act(t, "order", "filled", "buy", 100, 1),
            act(t, "order", "filled", "sell", 100, 2),
            act(t, "order", "filled", "buy", 100, 3),
            act(t, "order", "filled", "sell", 102, 4),
        ];
        assert_eq!(rule.evaluate(&window(), &balanced).len(), 1);
        let one_sided = vec![
            act(t, "order", "filled", "buy", 100, 1),
            act(t, "order", "filled", "buy", 100, 2),
            act(t, "order", "filled", "buy", 100, 3),
            act(t, "order", "filled", "sell", 10, 4),
        ];
        assert!(rule.evaluate(&window(), &one_sided).is_empty());
    }

    #[test]
    fn quote_probing_fires_on_low_execution_only() {
        let t = Uuid::new_v4();
        let rule = QuoteProbing {
            min_quotes: 50,
            max_exec_ratio: 0.02,
        };
        let mut probing: Vec<_> = (0..60)
            .map(|i| act(t, "quote", "quoted", "buy", 1, i))
            .collect();
        assert_eq!(rule.evaluate(&window(), &probing).len(), 1);
        // near miss: healthy conversion
        probing.extend((0..10).map(|i| act(t, "order", "filled", "buy", 1, 100 + i)));
        assert!(rule.evaluate(&window(), &probing).is_empty());
        // near miss: too few quotes
        assert!(rule.evaluate(&window(), &probing[..10]).is_empty());
    }

    #[test]
    fn volume_spike_needs_spike_and_favorable_price() {
        let t = Uuid::new_v4();
        let rule = VolumeSpikeFavorablePricing {
            multiplier: 5.0,
            min_volume: 1000.0,
            favorable_bps: 25.0,
            baseline_windows: 4,
        };
        let priced = |price: i64| {
            let mut x = act(t, "order", "filled", "buy", 5000, 60);
            x.price = Some(BigDecimal::from(price));
            x.ref_price = Some(BigDecimal::from(100));
            x
        };
        // baseline: 100 per window over 4 windows
        let mut base: Vec<_> = (1..=4)
            .map(|i| act(t, "order", "filled", "buy", 100, -3600 * i + 10))
            .collect();
        base.push(priced(98)); // 200 bps below reference
        assert_eq!(rule.evaluate(&window(), &base).len(), 1);
        // near miss: spike but unfavorable price
        base.pop();
        base.push(priced(100));
        assert!(rule.evaluate(&window(), &base).is_empty());
    }

    #[test]
    fn dedupe_key_is_stable_per_window() {
        let t = Uuid::new_v4();
        let rule = QuoteProbing {
            min_quotes: 5,
            max_exec_ratio: 0.5,
        };
        let acts: Vec<_> = (0..6)
            .map(|i| act(t, "quote", "quoted", "buy", 1, i))
            .collect();
        let a = rule.evaluate(&window(), &acts);
        let b = rule.evaluate(&window(), &acts);
        assert_eq!(a[0].dedupe_key, b[0].dedupe_key);
    }
}
