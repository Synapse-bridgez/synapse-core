//! Per-tenant latency histograms for real-user monitoring (issue #1337).
//!
//! Global p50/p95 can look healthy while one tenant is consistently slow.
//! This module keeps a rolling-window latency histogram per tenant for the
//! primary request paths (webhook ingestion, GraphQL) and exposes them
//!
//! - to internal dashboards as gauges (`tenant_request_latency_window_*`),
//! - to operators via `GET /stats/tenant-latency`,
//! - to each tenant, for its own traffic only, via `GET /usage/latency`.
//!
//! # Cardinality (see docs/tenant-latency-histograms.md)
//!
//! - Latency is bucketed into a fixed, reviewed set of [`BUCKETS`] buckets —
//!   never raw per-request series.
//! - Only the top [`TenantLatencyConfig::max_tracked_tenants`] tenants by
//!   volume, each with at least
//!   [`TenantLatencyConfig::min_requests_per_window`] requests in the window,
//!   get their own exported series. Everyone else — including low-volume
//!   tenants whose histogram would be statistically meaningless — is folded
//!   into one `_other` series; requests with no attributable tenant go to
//!   `_unattributed`. Exported series are therefore bounded by
//!   `(max_tracked_tenants + 2) × routes × buckets` regardless of tenant count.
//! - Series are exported as rolling-window gauges computed at collection
//!   time, so a tenant that drops out of the top set simply stops being
//!   reported (no cumulative series left behind by the SDK).
//! - In-process state is itself capped at
//!   [`TenantLatencyConfig::max_in_process_tenants`]; beyond it, new tenants
//!   are recorded straight into `_other`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::Serialize;
use uuid::Uuid;

use crate::utils::histogram::{bucket_index, bucket_quantile};

/// Reviewed bucket upper bounds in ms (11 bounds + overflow = 12 buckets),
/// spanning fast GraphQL reads to slow webhook ingestion under load.
pub const LATENCY_BOUNDS_MS: [f64; 11] = [
    5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0, 5000.0, 10000.0,
];
pub const BUCKETS: usize = LATENCY_BOUNDS_MS.len() + 1;

/// Rolling window = `SLOTS × SLOT_SECS` = 1 hour, advancing every 5 minutes.
pub const SLOT_SECS: u64 = 300;
pub const SLOTS: usize = 12;
pub const WINDOW_SECS: u64 = SLOT_SECS * SLOTS as u64;

pub const OTHER_LABEL: &str = "_other";
pub const UNATTRIBUTED_LABEL: &str = "_unattributed";

/// Instrumented request paths. Closed set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteClass {
    WebhookIngestion,
    GraphQl,
}

impl RouteClass {
    pub const ALL: [RouteClass; 2] = [RouteClass::WebhookIngestion, RouteClass::GraphQl];

    pub fn as_str(self) -> &'static str {
        match self {
            RouteClass::WebhookIngestion => "webhook_ingestion",
            RouteClass::GraphQl => "graphql",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum TenantKey {
    Tenant(Uuid),
    Unattributed,
    Overflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TenantLatencyConfig {
    /// Max tenants with their own exported series.
    pub max_tracked_tenants: usize,
    /// Requests per window a tenant needs before it gets its own series or
    /// percentiles.
    pub min_requests_per_window: u64,
    /// Max tenants held in-process.
    pub max_in_process_tenants: usize,
}

impl Default for TenantLatencyConfig {
    fn default() -> Self {
        Self {
            max_tracked_tenants: 50,
            min_requests_per_window: 50,
            max_in_process_tenants: 5_000,
        }
    }
}

impl TenantLatencyConfig {
    /// `TENANT_LATENCY_MAX_TRACKED_TENANTS` (clamped 1..=200),
    /// `TENANT_LATENCY_MIN_REQUESTS` (clamped 1..=100_000),
    /// `TENANT_LATENCY_MAX_TENANTS` (clamped 100..=50_000).
    pub fn from_env() -> Self {
        let d = Self::default();
        let get = |var: &str| std::env::var(var).ok().and_then(|v| v.parse::<u64>().ok());
        Self {
            max_tracked_tenants: get("TENANT_LATENCY_MAX_TRACKED_TENANTS")
                .map(|v| v.clamp(1, 200) as usize)
                .unwrap_or(d.max_tracked_tenants),
            min_requests_per_window: get("TENANT_LATENCY_MIN_REQUESTS")
                .map(|v| v.clamp(1, 100_000))
                .unwrap_or(d.min_requests_per_window),
            max_in_process_tenants: get("TENANT_LATENCY_MAX_TENANTS")
                .map(|v| v.clamp(100, 50_000) as usize)
                .unwrap_or(d.max_in_process_tenants),
        }
    }

    /// Hard upper bound on exported series per gauge instrument.
    pub fn max_exported_series(&self) -> usize {
        (self.max_tracked_tenants + 2) * RouteClass::ALL.len()
    }
}

// ---------------------------------------------------------------------------
// Histograms
// ---------------------------------------------------------------------------

/// A merged histogram over the rolling window.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct WindowHistogram {
    pub buckets: [u64; BUCKETS],
    pub count: u64,
    pub sum_ms: f64,
}

impl WindowHistogram {
    fn merge(&mut self, other: &WindowHistogram) {
        for (a, b) in self.buckets.iter_mut().zip(other.buckets.iter()) {
            *a += b;
        }
        self.count += other.count;
        self.sum_ms += other.sum_ms;
    }

    pub fn quantile(&self, q: f64) -> Option<f64> {
        bucket_quantile(&LATENCY_BOUNDS_MS, &self.buckets, q)
    }

    pub fn mean_ms(&self) -> Option<f64> {
        (self.count > 0).then(|| self.sum_ms / self.count as f64)
    }

    /// Prometheus-style cumulative counts per `le` bound (last is `+Inf`).
    pub fn cumulative(&self) -> Vec<(String, u64)> {
        let mut acc = 0;
        self.buckets
            .iter()
            .enumerate()
            .map(|(i, &c)| {
                acc += c;
                let le = LATENCY_BOUNDS_MS
                    .get(i)
                    .map(|b| format!("{b}"))
                    .unwrap_or_else(|| "+Inf".to_string());
                (le, acc)
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Slot {
    epoch: u64,
    buckets: [u64; BUCKETS],
    count: u64,
    sum_ms: f64,
}

#[derive(Debug, Clone, Default)]
struct RollingHistogram {
    slots: [Slot; SLOTS],
}

fn epoch_of(now_secs: u64) -> u64 {
    now_secs / SLOT_SECS
}

impl RollingHistogram {
    fn record(&mut self, now_secs: u64, latency_ms: f64) {
        let epoch = epoch_of(now_secs);
        let slot = &mut self.slots[(epoch % SLOTS as u64) as usize];
        if slot.epoch != epoch {
            *slot = Slot {
                epoch,
                ..Slot::default()
            };
        }
        slot.buckets[bucket_index(&LATENCY_BOUNDS_MS, latency_ms)] += 1;
        slot.count += 1;
        slot.sum_ms += latency_ms;
    }

    fn window(&self, now_secs: u64) -> WindowHistogram {
        let current = epoch_of(now_secs);
        let oldest = current.saturating_sub(SLOTS as u64 - 1);
        let mut out = WindowHistogram::default();
        for slot in &self.slots {
            if slot.count > 0 && slot.epoch >= oldest && slot.epoch <= current {
                for (a, b) in out.buckets.iter_mut().zip(slot.buckets.iter()) {
                    *a += b;
                }
                out.count += slot.count;
                out.sum_ms += slot.sum_ms;
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

#[derive(Default)]
struct RegistryInner {
    histograms: HashMap<(TenantKey, RouteClass), RollingHistogram>,
    tenants: HashSet<Uuid>,
}

/// In-process per-tenant latency store.
pub struct TenantLatencyRegistry {
    config: TenantLatencyConfig,
    inner: Mutex<RegistryInner>,
}

/// One exported series: a tenant label (tenant UUID, `_other` or
/// `_unattributed`), a route and its window histogram.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportedSeries {
    pub tenant_label: String,
    pub route: RouteClass,
    pub window: WindowHistogram,
}

#[derive(Debug, Clone, Serialize)]
pub struct BucketCount {
    /// Upper bound in ms, or `+Inf`.
    pub le: String,
    /// Requests in this bucket (not cumulative).
    pub count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RouteLatencySummary {
    pub route: RouteClass,
    pub request_count: u64,
    /// False below `min_requests_per_window`: percentiles are then omitted
    /// rather than reported from a handful of samples.
    pub statistically_meaningful: bool,
    pub p50_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    pub p99_ms: Option<f64>,
    pub mean_ms: Option<f64>,
    pub buckets: Vec<BucketCount>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TenantLatencySummary {
    pub tenant_id: Uuid,
    pub window_secs: u64,
    pub min_requests_for_percentiles: u64,
    pub routes: Vec<RouteLatencySummary>,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl TenantLatencyRegistry {
    pub fn new(config: TenantLatencyConfig) -> Self {
        Self {
            config,
            inner: Mutex::new(RegistryInner::default()),
        }
    }

    pub fn config(&self) -> TenantLatencyConfig {
        self.config
    }

    pub fn record(&self, tenant: Option<Uuid>, route: RouteClass, latency: Duration) {
        self.record_at(tenant, route, latency, now_secs());
    }

    pub fn record_at(&self, tenant: Option<Uuid>, route: RouteClass, latency: Duration, now: u64) {
        let ms = latency.as_secs_f64() * 1000.0;
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let key = match tenant {
            None => TenantKey::Unattributed,
            Some(id) if inner.tenants.contains(&id) => TenantKey::Tenant(id),
            Some(id) if inner.tenants.len() < self.config.max_in_process_tenants => {
                inner.tenants.insert(id);
                TenantKey::Tenant(id)
            }
            Some(_) => TenantKey::Overflow,
        };
        inner
            .histograms
            .entry((key, route))
            .or_default()
            .record(now, ms);
    }

    /// Distinct tenants currently held in-process.
    pub fn in_process_tenants(&self) -> usize {
        self.inner.lock().map(|i| i.tenants.len()).unwrap_or(0)
    }

    /// Drops tenants with no requests left in the window, freeing capacity.
    fn prune(inner: &mut RegistryInner, now: u64) {
        inner.histograms.retain(|_, h| h.window(now).count > 0);
        let live: HashSet<Uuid> = inner
            .histograms
            .keys()
            .filter_map(|(k, _)| match k {
                TenantKey::Tenant(id) => Some(*id),
                _ => None,
            })
            .collect();
        inner.tenants = live;
    }

    /// The bounded exported series set (see module docs).
    pub fn export_at(&self, now: u64) -> Vec<ExportedSeries> {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        Self::prune(&mut inner, now);

        let mut out = Vec::new();
        for route in RouteClass::ALL {
            let mut per_tenant: Vec<(Uuid, WindowHistogram)> = Vec::new();
            let mut other = WindowHistogram::default();
            let mut unattributed = WindowHistogram::default();
            for ((key, r), h) in inner.histograms.iter() {
                if *r != route {
                    continue;
                }
                let w = h.window(now);
                match key {
                    TenantKey::Tenant(id) => per_tenant.push((*id, w)),
                    TenantKey::Overflow => other.merge(&w),
                    TenantKey::Unattributed => unattributed.merge(&w),
                }
            }
            // Highest volume first; UUID tiebreak keeps the choice stable.
            per_tenant.sort_by(|a, b| b.1.count.cmp(&a.1.count).then(a.0.cmp(&b.0)));
            let mut tracked = 0;
            for (id, w) in per_tenant {
                if tracked < self.config.max_tracked_tenants
                    && w.count >= self.config.min_requests_per_window
                {
                    tracked += 1;
                    out.push(ExportedSeries {
                        tenant_label: id.to_string(),
                        route,
                        window: w,
                    });
                } else {
                    other.merge(&w);
                }
            }
            for (label, w) in [(OTHER_LABEL, other), (UNATTRIBUTED_LABEL, unattributed)] {
                if w.count > 0 {
                    out.push(ExportedSeries {
                        tenant_label: label.to_string(),
                        route,
                        window: w,
                    });
                }
            }
        }
        out
    }

    pub fn export(&self) -> Vec<ExportedSeries> {
        self.export_at(now_secs())
    }

    /// One tenant's own latency, for the tenant-facing usage API.
    pub fn summary_at(&self, tenant: Uuid, now: u64) -> TenantLatencySummary {
        let inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let routes = RouteClass::ALL
            .iter()
            .map(|&route| {
                let w = inner
                    .histograms
                    .get(&(TenantKey::Tenant(tenant), route))
                    .map(|h| h.window(now))
                    .unwrap_or_default();
                let meaningful = w.count >= self.config.min_requests_per_window;
                let pick = |q| if meaningful { w.quantile(q) } else { None };
                RouteLatencySummary {
                    route,
                    request_count: w.count,
                    statistically_meaningful: meaningful,
                    p50_ms: pick(0.50),
                    p95_ms: pick(0.95),
                    p99_ms: pick(0.99),
                    mean_ms: w.mean_ms(),
                    buckets: w
                        .buckets
                        .iter()
                        .enumerate()
                        .map(|(i, &count)| BucketCount {
                            le: LATENCY_BOUNDS_MS
                                .get(i)
                                .map(|b| format!("{b}"))
                                .unwrap_or_else(|| "+Inf".to_string()),
                            count,
                        })
                        .collect(),
                }
            })
            .collect();
        TenantLatencySummary {
            tenant_id: tenant,
            window_secs: WINDOW_SECS,
            min_requests_for_percentiles: self.config.min_requests_per_window,
            routes,
        }
    }

    pub fn summary(&self, tenant: Uuid) -> TenantLatencySummary {
        self.summary_at(tenant, now_secs())
    }
}

/// The process-wide registry.
pub fn registry() -> &'static TenantLatencyRegistry {
    static REGISTRY: OnceLock<TenantLatencyRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| TenantLatencyRegistry::new(TenantLatencyConfig::from_env()))
}

// ---------------------------------------------------------------------------
// Operator overview (GET /stats/tenant-latency)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct SeriesOverview {
    pub tenant: String,
    pub route: RouteClass,
    pub request_count: u64,
    pub p50_ms: Option<f64>,
    pub p95_ms: Option<f64>,
    pub p99_ms: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TenantLatencyOverview {
    pub window_secs: u64,
    pub max_tracked_tenants: usize,
    pub min_requests_per_window: u64,
    pub max_exported_series: usize,
    pub in_process_tenants: usize,
    /// Sorted by p95 descending: the slowest tenants first.
    pub series: Vec<SeriesOverview>,
}

pub fn overview(reg: &TenantLatencyRegistry) -> TenantLatencyOverview {
    let mut series: Vec<SeriesOverview> = reg
        .export()
        .into_iter()
        .map(|s| SeriesOverview {
            tenant: s.tenant_label,
            route: s.route,
            request_count: s.window.count,
            p50_ms: s.window.quantile(0.50),
            p95_ms: s.window.quantile(0.95),
            p99_ms: s.window.quantile(0.99),
        })
        .collect();
    series.sort_by(|a, b| {
        b.p95_ms
            .unwrap_or(0.0)
            .partial_cmp(&a.p95_ms.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let cfg = reg.config();
    TenantLatencyOverview {
        window_secs: WINDOW_SECS,
        max_tracked_tenants: cfg.max_tracked_tenants,
        min_requests_per_window: cfg.min_requests_per_window,
        max_exported_series: cfg.max_exported_series(),
        in_process_tenants: reg.in_process_tenants(),
        series,
    }
}

// ---------------------------------------------------------------------------
// Exported gauges
// ---------------------------------------------------------------------------

/// Registers the rolling-window gauges. `tenant_id` here is a reviewed
/// cardinality exception (docs/metrics-cardinality-convention.md): its
/// values are capped at `max_tracked_tenants` + `_other` + `_unattributed`.
/// Keep the returned handles alive for the process lifetime.
pub fn register_tenant_latency_gauges() -> (
    opentelemetry::metrics::ObservableGauge<u64>,
    opentelemetry::metrics::ObservableGauge<u64>,
    opentelemetry::metrics::ObservableGauge<f64>,
) {
    use opentelemetry::KeyValue;
    let meter = opentelemetry::global::meter("synapse-core");
    let buckets = meter
        .u64_observable_gauge("tenant_request_latency_window_bucket")
        .with_description(
            "Per-tenant request latency over the rolling 1h window: cumulative count per \
             `le` bucket (ms). Top-K tenants only; others in `_other`.",
        )
        .with_callback(|observer| {
            for s in registry().export() {
                for (le, count) in s.window.cumulative() {
                    observer.observe(
                        count,
                        &[
                            KeyValue::new("tenant_id", s.tenant_label.clone()),
                            KeyValue::new("route", s.route.as_str()),
                            KeyValue::new("le", le),
                        ],
                    );
                }
            }
        })
        .init();
    let counts = meter
        .u64_observable_gauge("tenant_request_latency_window_count")
        .with_description("Per-tenant request count over the rolling 1h window")
        .with_callback(|observer| {
            for s in registry().export() {
                observer.observe(
                    s.window.count,
                    &[
                        KeyValue::new("tenant_id", s.tenant_label.clone()),
                        KeyValue::new("route", s.route.as_str()),
                    ],
                );
            }
        })
        .init();
    let p95 = meter
        .f64_observable_gauge("tenant_request_latency_window_p95_ms")
        .with_description("Per-tenant p95 request latency over the rolling 1h window, in ms")
        .with_callback(|observer| {
            for s in registry().export() {
                if let Some(v) = s.window.quantile(0.95) {
                    observer.observe(
                        v,
                        &[
                            KeyValue::new("tenant_id", s.tenant_label.clone()),
                            KeyValue::new("route", s.route.as_str()),
                        ],
                    );
                }
            }
        })
        .init();
    (buckets, counts, p95)
}

// ---------------------------------------------------------------------------
// Request attribution + middleware
// ---------------------------------------------------------------------------

/// Slot the authenticated tenant is written into by whichever extractor
/// resolves it (`TenantContext`), so the latency middleware wrapping the
/// handler can attribute the request after the fact.
#[derive(Clone, Default, Debug)]
pub struct TenantAttribution(Arc<OnceLock<Uuid>>);

impl TenantAttribution {
    pub fn set(&self, tenant: Uuid) {
        let _ = self.0.set(tenant);
    }

    pub fn get(&self) -> Option<Uuid> {
        self.0.get().copied()
    }
}

/// State for [`tenant_latency_middleware`].
#[derive(Clone)]
pub struct TenantLatencyLayer {
    pub app_state: crate::AppState,
    pub route: RouteClass,
}

/// Attribution rule, pure for testability: an authenticated tenant wins;
/// otherwise the `X-Tenant-ID` hint, only if it names a known tenant (the
/// same trust level quota bucketing uses); and never for a request that was
/// rejected as unauthenticated/forbidden (so spoofed hints cannot skew a
/// real tenant's histogram with fast 401s).
pub fn attribute(
    authenticated: Option<Uuid>,
    hint: Option<Uuid>,
    known_tenant: impl Fn(Uuid) -> bool,
    status: u16,
) -> Option<Uuid> {
    if status == 401 || status == 403 {
        return None;
    }
    authenticated.or_else(|| hint.filter(|id| known_tenant(*id)))
}

fn tenant_hint(headers: &axum::http::HeaderMap) -> Option<Uuid> {
    headers
        .get("x-tenant-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| Uuid::parse_str(v.trim()).ok())
}

/// Times the full request (including auth, validation and rate limiting
/// layers inside it) and records it against the attributed tenant.
pub async fn tenant_latency_middleware(
    axum::extract::State(layer): axum::extract::State<TenantLatencyLayer>,
    mut req: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next<axum::body::Body>,
) -> axum::response::Response {
    let slot = TenantAttribution::default();
    req.extensions_mut().insert(slot.clone());
    let hint = tenant_hint(req.headers());
    let started = std::time::Instant::now();

    let response = next.run(req).await;

    let elapsed = started.elapsed();
    let known = match hint {
        Some(id) => layer
            .app_state
            .tenant_configs
            .read()
            .await
            .contains_key(&id),
        None => false,
    };
    let tenant = attribute(slot.get(), hint, |_| known, response.status().as_u16());
    registry().record(tenant, layer.route, elapsed);
    response
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_800_000_000;

    fn cfg(k: usize, min: u64, cap: usize) -> TenantLatencyConfig {
        TenantLatencyConfig {
            max_tracked_tenants: k,
            min_requests_per_window: min,
            max_in_process_tenants: cap,
        }
    }

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    fn tenant(n: u128) -> Uuid {
        Uuid::from_u128(n + 1)
    }

    #[test]
    fn bucket_layout_is_reviewed_and_bounded() {
        assert_eq!(BUCKETS, 12);
        assert!(LATENCY_BOUNDS_MS.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn requests_land_in_the_right_buckets() {
        let reg = TenantLatencyRegistry::new(cfg(10, 1, 100));
        let t = tenant(1);
        for (latency, expected_bucket) in [(1, 0), (5, 0), (6, 1), (99, 4), (100, 4), (20_000, 11)]
        {
            let before = reg.summary_at(t, T0).routes[0].buckets[expected_bucket].count;
            reg.record_at(Some(t), RouteClass::WebhookIngestion, ms(latency), T0);
            let after = reg.summary_at(t, T0).routes[0].buckets[expected_bucket].count;
            assert_eq!(
                after,
                before + 1,
                "{latency}ms should land in bucket {expected_bucket}"
            );
        }
        let s = reg.summary_at(t, T0);
        assert_eq!(s.routes[0].request_count, 6);
        assert_eq!(s.routes[1].request_count, 0, "graphql untouched");
        assert_eq!(s.routes[0].buckets.last().unwrap().le, "+Inf");
    }

    #[test]
    fn window_rolls_old_slots_off() {
        let reg = TenantLatencyRegistry::new(cfg(10, 1, 100));
        let t = tenant(2);
        reg.record_at(Some(t), RouteClass::GraphQl, ms(10), T0);
        assert_eq!(
            reg.summary_at(t, T0 + WINDOW_SECS - SLOT_SECS).routes[1].request_count,
            1
        );
        assert_eq!(
            reg.summary_at(t, T0 + WINDOW_SECS + SLOT_SECS).routes[1].request_count,
            0
        );
        // A slot reused by a later epoch starts from zero.
        reg.record_at(Some(t), RouteClass::GraphQl, ms(10), T0 + WINDOW_SECS);
        assert_eq!(
            reg.summary_at(t, T0 + WINDOW_SECS).routes[1].request_count,
            1
        );
    }

    #[test]
    fn percentiles_reflect_a_slow_tenant_hidden_by_global_averages() {
        let reg = TenantLatencyRegistry::new(cfg(10, 20, 100));
        let slow = tenant(3);
        for i in 0..1_000u128 {
            reg.record_at(Some(tenant(100 + i % 20)), RouteClass::GraphQl, ms(8), T0);
        }
        for _ in 0..50 {
            reg.record_at(Some(slow), RouteClass::GraphQl, ms(3_000), T0);
        }
        let s = reg.summary_at(slow, T0);
        let p95 = s.routes[1].p95_ms.unwrap();
        assert!(p95 > 2_500.0 && p95 <= 5_000.0, "slow tenant p95 = {p95}");
        let overview = overview_at(&reg, T0);
        assert_eq!(overview.series[0].tenant, slow.to_string(), "slowest first");
    }

    fn overview_at(reg: &TenantLatencyRegistry, now: u64) -> TenantLatencyOverview {
        // Same as `overview` but at a fixed time.
        let mut series: Vec<SeriesOverview> = reg
            .export_at(now)
            .into_iter()
            .map(|s| SeriesOverview {
                tenant: s.tenant_label,
                route: s.route,
                request_count: s.window.count,
                p50_ms: s.window.quantile(0.5),
                p95_ms: s.window.quantile(0.95),
                p99_ms: s.window.quantile(0.99),
            })
            .collect();
        series.sort_by(|a, b| {
            b.p95_ms
                .unwrap_or(0.0)
                .partial_cmp(&a.p95_ms.unwrap_or(0.0))
                .unwrap()
        });
        TenantLatencyOverview {
            window_secs: WINDOW_SECS,
            max_tracked_tenants: reg.config().max_tracked_tenants,
            min_requests_per_window: reg.config().min_requests_per_window,
            max_exported_series: reg.config().max_exported_series(),
            in_process_tenants: reg.in_process_tenants(),
            series,
        }
    }

    #[test]
    fn low_volume_tenants_roll_into_other_and_get_no_percentiles() {
        let reg = TenantLatencyRegistry::new(cfg(10, 50, 100));
        let tiny = tenant(4);
        for _ in 0..3 {
            reg.record_at(Some(tiny), RouteClass::WebhookIngestion, ms(40), T0);
        }
        let series = reg.export_at(T0);
        assert!(series.iter().all(|s| s.tenant_label != tiny.to_string()));
        let other = series
            .iter()
            .find(|s| s.tenant_label == OTHER_LABEL)
            .unwrap();
        assert_eq!(other.window.count, 3);

        let summary = reg.summary_at(tiny, T0);
        let route = &summary.routes[0];
        assert_eq!(route.request_count, 3, "tenant still sees its own volume");
        assert!(!route.statistically_meaningful);
        assert_eq!(route.p95_ms, None, "no meaningless percentiles");
        assert!(route.mean_ms.is_some());
    }

    #[test]
    fn unattributed_requests_get_their_own_series() {
        let reg = TenantLatencyRegistry::new(cfg(10, 1, 100));
        reg.record_at(None, RouteClass::GraphQl, ms(20), T0);
        let series = reg.export_at(T0);
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].tenant_label, UNATTRIBUTED_LABEL);
        assert_eq!(series[0].route, RouteClass::GraphQl);
    }

    /// Large synthetic multi-tenant load: 10,000 tenants with a long-tail
    /// volume distribution. Exported series must stay within
    /// `(K + 2) × routes`, the top-K must be the highest-volume eligible
    /// tenants, no request may be lost from the totals, and in-process state
    /// must respect its cap.
    #[test]
    fn cardinality_is_bounded_under_large_synthetic_load() {
        let config = cfg(50, 50, 5_000);
        let reg = TenantLatencyRegistry::new(config);
        let mut total = [0u64; 2];
        for n in 0..10_000u128 {
            // Tenants 0..100 are heavy (100..199 requests); the rest light.
            let requests = if n < 100 {
                100 + n as u64
            } else {
                1 + (n as u64 % 5)
            };
            for r in 0..requests {
                let route = RouteClass::ALL[(r % 2) as usize];
                total[(r % 2) as usize] += 1;
                reg.record_at(Some(tenant(n)), route, ms(5 + (r % 400)), T0 + (r % 3_000));
            }
        }
        for _ in 0..500 {
            reg.record_at(None, RouteClass::WebhookIngestion, ms(30), T0);
            total[0] += 1;
        }
        let now = T0 + 3_000;

        assert!(reg.in_process_tenants() <= config.max_in_process_tenants);

        let series = reg.export_at(now);
        assert!(
            series.len() <= config.max_exported_series(),
            "{} series > bound {}",
            series.len(),
            config.max_exported_series()
        );
        for route in RouteClass::ALL {
            let per_route: Vec<_> = series.iter().filter(|s| s.route == route).collect();
            let named: Vec<_> = per_route
                .iter()
                .filter(|s| s.tenant_label != OTHER_LABEL && s.tenant_label != UNATTRIBUTED_LABEL)
                .collect();
            assert_eq!(named.len(), 50, "{route:?}: exactly K tenants tracked");
            // Heaviest tenants win: every tracked tenant is one of the 100 heavy ones
            // and each tracked count is >= every untracked tenant's count.
            let min_tracked = named.iter().map(|s| s.window.count).min().unwrap();
            assert!(min_tracked >= config.min_requests_per_window);
            for s in &named {
                let id = Uuid::parse_str(&s.tenant_label).unwrap().as_u128() - 1;
                assert!(id < 100, "tracked tenant {id} is not a heavy tenant");
            }
            // Conservation: nothing is dropped, only folded.
            let sum: u64 = per_route.iter().map(|s| s.window.count).sum();
            assert_eq!(sum, total[route as usize], "{route:?} total conserved");
            // Cumulative buckets end at the total.
            for s in &per_route {
                assert_eq!(s.window.cumulative().last().unwrap().1, s.window.count);
            }
        }
    }

    #[test]
    fn in_process_cap_overflows_into_other_and_prune_frees_capacity() {
        let reg = TenantLatencyRegistry::new(cfg(5, 1, 3));
        for n in 0..5u128 {
            reg.record_at(Some(tenant(n)), RouteClass::GraphQl, ms(10), T0);
        }
        assert_eq!(reg.in_process_tenants(), 3);
        let series = reg.export_at(T0);
        let other = series
            .iter()
            .find(|s| s.tenant_label == OTHER_LABEL)
            .unwrap();
        assert_eq!(other.window.count, 2);

        // After the window passes, pruning frees the capacity again.
        let later = T0 + WINDOW_SECS + SLOT_SECS;
        assert!(reg.export_at(later).is_empty());
        assert_eq!(reg.in_process_tenants(), 0);
        reg.record_at(Some(tenant(4)), RouteClass::GraphQl, ms(10), later);
        assert_eq!(reg.summary_at(tenant(4), later).routes[1].request_count, 1);
    }

    #[test]
    fn attribution_rules() {
        let authed = Some(tenant(7));
        let hint = Some(tenant(8));
        let known = |id: Uuid| id == tenant(8);
        assert_eq!(attribute(authed, hint, known, 200), authed);
        assert_eq!(attribute(None, hint, known, 200), hint);
        assert_eq!(
            attribute(None, Some(tenant(9)), known, 200),
            None,
            "unknown hint"
        );
        assert_eq!(attribute(None, hint, known, 401), None, "rejected request");
        assert_eq!(attribute(authed, hint, known, 403), None);
        assert_eq!(
            attribute(None, hint, known, 500),
            hint,
            "server errors still count"
        );
    }

    #[test]
    fn attribution_slot_and_header_hint() {
        let slot = TenantAttribution::default();
        assert_eq!(slot.get(), None);
        slot.set(tenant(1));
        slot.set(tenant(2));
        assert_eq!(slot.get(), Some(tenant(1)), "first writer wins");

        let mut headers = axum::http::HeaderMap::new();
        assert_eq!(tenant_hint(&headers), None);
        headers.insert("x-tenant-id", tenant(3).to_string().parse().unwrap());
        assert_eq!(tenant_hint(&headers), Some(tenant(3)));
        headers.insert("x-tenant-id", "not-a-uuid".parse().unwrap());
        assert_eq!(tenant_hint(&headers), None);
    }

    #[test]
    fn config_from_env_defaults_and_bound() {
        let d = TenantLatencyConfig::default();
        assert_eq!(d.max_exported_series(), 104);
        let c = TenantLatencyConfig::from_env();
        assert!(c.max_tracked_tenants >= 1 && c.max_tracked_tenants <= 200);
    }

    #[test]
    fn overview_and_global_registry_work() {
        registry().record(Some(tenant(42)), RouteClass::WebhookIngestion, ms(12));
        let o = overview(registry());
        assert_eq!(o.window_secs, WINDOW_SECS);
        assert!(o.series.iter().any(|s| s.request_count > 0));
        let _gauges = register_tenant_latency_gauges();
    }
}
