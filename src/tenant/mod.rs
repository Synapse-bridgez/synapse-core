use axum::{
    async_trait,
    extract::{FromRef, FromRequestParts},
    http::{request::Parts, HeaderMap},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{error::AppError, AppState};

pub mod latency;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct TenantConfig {
    pub tenant_id: Uuid,
    pub name: String,
    pub webhook_secret: String,
    pub stellar_account: String,
    pub rate_limit_per_minute: i32,
    pub is_active: bool,
    #[serde(default = "default_idempotency_ttl")]
    pub idempotency_ttl_seconds: i64,
}

fn default_idempotency_ttl() -> i64 {
    86400 // 24 hours by default
}

#[derive(Debug, Clone)]
pub struct TenantContext {
    pub tenant_id: Uuid,
    pub config: TenantConfig,
}

impl TenantContext {
    pub fn new(tenant_id: Uuid, config: TenantConfig) -> Self {
        Self { tenant_id, config }
    }
}

/// Bounded latency histogram buckets (in milliseconds) used for per-tenant
/// real-user-monitoring views. The bucket count is deliberately fixed and
/// small so that per-tenant series stay cheap: `TENANT_LATENCY_BUCKETS.len()`
/// series per tenant per request path, rather than an unbounded raw-latency
/// series. Keep this list reviewed and stable — changing it changes the
/// cardinality budget for every tenant.
pub const TENANT_LATENCY_BUCKETS_MS: [u64; 8] =
    [5, 10, 25, 50, 100, 250, 500, 1000];

/// Minimum number of observations a tenant must accumulate within the current
/// roll-off window before its per-tenant histogram is considered statistically
/// meaningful. Tenants below this threshold are aggregated into a shared
/// `__low_volume__` bucket instead of emitting their own series, so a tenant
/// with ~3 requests/day does not produce a cardinality-expensive but
/// meaningless histogram.
pub const TENANT_LATENCY_MIN_OBSERVATIONS: u64 = 20;

/// Sentinel tenant label used to roll up very-low-volume tenants so they do
/// not each consume their own histogram series.
pub const LOW_VOLUME_TENANT_LABEL: &str = "__low_volume__";

/// Per-tenant latency histogram for a single request path.
///
/// `buckets[i]` counts observations whose latency is `<= TENANT_LATENCY_BUCKETS_MS[i]`
/// (cumulative, Prometheus-style), and `buckets[TENANT_LATENCY_BUCKETS_MS.len()]`
/// counts observations above the last bound. `count` and `sum_ms` are tracked
/// alongside so p50/p95 can be derived without storing raw samples.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TenantLatencyHistogram {
    pub buckets: [u64; TENANT_LATENCY_BUCKETS_MS.len() + 1],
    pub count: u64,
    pub sum_ms: u64,
}

impl TenantLatencyHistogram {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a single latency observation, placing it in the first bucket
    /// whose upper bound is `>= latency_ms`.
    pub fn observe(&mut self, latency_ms: u64) {
        let idx = TENANT_LATENCY_BUCKETS_MS
            .iter()
            .position(|&bound| latency_ms <= bound)
            .unwrap_or(TENANT_LATENCY_BUCKETS_MS.len());
        self.buckets[idx] += 1;
        self.count += 1;
        self.sum_ms = self.sum_ms.saturating_add(latency_ms);
    }

    /// Whether this histogram has enough observations to be reported as its
    /// own per-tenant series rather than rolled into the low-volume bucket.
    pub fn is_reportable(&self) -> bool {
        self.count >= TENANT_LATENCY_MIN_OBSERVATIONS
    }

    /// Merge another histogram into this one (used when rolling low-volume
    /// tenants into the shared sentinel series).
    pub fn merge(&mut self, other: &TenantLatencyHistogram) {
        for (dst, src) in self.buckets.iter_mut().zip(other.buckets.iter()) {
            *dst += *src;
        }
        self.count += other.count;
        self.sum_ms = self.sum_ms.saturating_add(other.sum_ms);
    }
}

/// The request paths that get their own per-tenant histogram. Kept as an enum
/// (rather than a free-form string label) so the set of emitted series is
/// bounded and reviewed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TenantLatencyPath {
    WebhookIngestion,
    GraphqlQuery,
}

impl TenantLatencyPath {
    pub fn as_label(&self) -> &'static str {
        match self {
            TenantLatencyPath::WebhookIngestion => "webhook_ingestion",
            TenantLatencyPath::GraphqlQuery => "graphql_query",
        }
    }
}

/// Per-tenant latency histograms keyed by request path. This is the in-memory
/// store backing both the internal dashboards and the tenant-facing quota/usage
/// API; it is intentionally bounded by the fixed path set and bucket count.
#[derive(Debug, Default)]
pub struct TenantLatencyStore {
    webhook_ingestion: std::collections::HashMap<Uuid, TenantLatencyHistogram>,
    graphql_query: std::collections::HashMap<Uuid, TenantLatencyHistogram>,
}

impl TenantLatencyStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn map_mut(
        &mut self,
        path: TenantLatencyPath,
    ) -> &mut std::collections::HashMap<Uuid, TenantLatencyHistogram> {
        match path {
            TenantLatencyPath::WebhookIngestion => &mut self.webhook_ingestion,
            TenantLatencyPath::GraphqlQuery => &mut self.graphql_query,
        }
    }

    fn map(
        &self,
        path: TenantLatencyPath,
    ) -> &std::collections::HashMap<Uuid, TenantLatencyHistogram> {
        match path {
            TenantLatencyPath::WebhookIngestion => &self.webhook_ingestion,
            TenantLatencyPath::GraphqlQuery => &self.graphql_query,
        }
    }

    /// Record a latency observation for a tenant on a given request path.
    pub fn observe(&mut self, tenant_id: Uuid, path: TenantLatencyPath, latency_ms: u64) {
        self.map_mut(path)
            .entry(tenant_id)
            .or_default()
            .observe(latency_ms);
    }

    /// Snapshot the per-tenant histograms for a path, rolling very-low-volume
    /// tenants into the shared `__low_volume__` sentinel so the emitted series
    /// count stays bounded regardless of how many tenants exist.
    pub fn snapshot(&self, path: TenantLatencyPath) -> Vec<(String, TenantLatencyHistogram)> {
        let mut out: Vec<(String, TenantLatencyHistogram)> = Vec::new();
        let mut low_volume = TenantLatencyHistogram::new();

        for (tenant_id, hist) in self.map(path) {
            if hist.is_reportable() {
                out.push((tenant_id.to_string(), hist.clone()));
            } else {
                low_volume.merge(hist);
            }
        }

        if low_volume.count > 0 {
            out.push((LOW_VOLUME_TENANT_LABEL.to_string(), low_volume));
        }

        out
    }
}

// Generic over any router state `S` that can hand us an `AppState` — this is
// what lets the extractor be used both directly (routers keyed on AppState,
// e.g. /ws, /reconnect) and via the substate pattern (routers keyed on
// ApiState, e.g. the tenant-scoped data routes in create_app), without a
// separate impl for each.
#[async_trait]
impl<S> FromRequestParts<S> for TenantContext
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &S,
    ) -> std::result::Result<Self, AppError> {
        let state = AppState::from_ref(state);
        let tenant_id = match resolve_tenant_id(parts, &state).await {
            Ok(id) => id,
            Err(e) => {
                tracing::warn!(
                    counter.unauthenticated_rejections_total = 1u64,
                    error = %e,
                    "TenantContext: rejecting unauthenticated/invalid-credential request"
                );
                return Err(e);
            }
        };

        let config = state
            .get_tenant_config(tenant_id)
            .await
            .ok_or(AppError::TenantNotFound)?;

        if !config.is_active {
            return Err(AppError::Unauthorized("tenant inactive".to_string()));
        }

        // Let a wrapping tenant-latency middleware (#1337) attribute this
        // request to the tenant that actually authenticated.
        if let Some(slot) = parts.extensions.get::<latency::TenantAttribution>() {
            slot.set(tenant_id);
        }

        Ok(TenantContext::new(tenant_id, config))
    }
}

async fn resolve_tenant_id(
    parts: &mut Parts,
    state: &AppState,
) -> std::result::Result<Uuid, AppError> {
    // NOTE: this used to try `Path<Uuid>` first and return whatever UUID it
    // found in the URL as the tenant_id — which on a route like
    // /transactions/:id would consume the *transaction* ID and return early,
    // before ever checking for an API key. Every request would then fail
    // tenant-config lookup and return 404/TenantNotFound regardless of
    // whether a valid credential was supplied, since the real API-key branch
    // below was unreachable. Nothing in this codebase legitimately depends
    // on resolving tenant identity from an arbitrary path UUID, so that
    // branch is removed rather than route-conditioned.
    let headers = &parts.headers;

    // NOTE: `X-Tenant-ID` is intentionally NOT accepted as a standalone
    // credential here. It used to be — this extractor previously resolved
    // tenant identity from a bare, client-supplied `X-Tenant-ID` header with
    // no proof of authorization at all, which would have let any caller
    // impersonate any tenant by guessing a UUID. `X-Tenant-ID` is still used
    // elsewhere in this codebase (quota bucketing, idempotency-key
    // namespacing) where an unverified hint is an acceptable input, but not
    // here: this extractor is what real handlers use to decide whose data to
    // return, so it must only trust a credential that was actually looked up
    // against the `tenants` table.
    if let Some(api_key) = extract_api_key(headers) {
        // Only failed lookups count against the brute-force budget — see
        // middleware::auth::admin_auth's doc comment for why: a valid key
        // used many times is routine API traffic (already throttled
        // separately and much more generously by
        // middleware::quota::rate_limit_middleware), not a guessing attack.
        match resolve_tenant_by_api_key(&state.db, &api_key).await {
            Ok(tenant_id) => return Ok(tenant_id),
            Err(e) => {
                let source_ip = parts
                    .extensions
                    .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
                    .map(|ci| ci.0.to_string())
                    .unwrap_or_else(|| "unknown".to_string());

                if crate::auth::rate_limiting::TENANT_AUTH_RATE_LIMITER
                    .check_auth_rate_limit(&format!("ip:{source_ip}"))
                    .is_err()
                {
                    tracing::warn!(
                        counter.tenant_auth_lockout_triggered_total = 1u64,
                        source_ip = %source_ip,
                        "TenantContext: rate limit exceeded"
                    );
                    return Err(AppError::RateLimitExceeded);
                }

                return Err(e);
            }
        }
    }

    Err(AppError::InvalidApiKey)
}

fn extract_api_key(headers: &HeaderMap) -> Option<String> {
    headers
        .get("X-API-Key")
        .or_else(|| headers.get("Authorization"))
        .and_then(|v| v.to_str().ok())
        .map(|s| {
            if s.starts_with("Bearer ") {
                s.trim_start_matches("Bearer ").to_string()
            } else {
                s.to_string()
            }
        })
}

async fn resolve_tenant_by_api_key(
    pool: &sqlx::PgPool,
    api_key: &str,
) -> std::result::Result<Uuid, AppError> {
    use sqlx::Row;
    let hash = crate::db::queries::hash_api_key(api_key);
    let row = sqlx::query(
        "SELECT tenant_id FROM tenants WHERE (api_key_hash = $1 OR (previous_api_key_hash = $1 AND grace_period_expires_at > NOW()))",
    )
    .bind(hash)
    .fetch_optional(pool)
    .await?;

    if let Some(r) = row {
        let tenant_id: Uuid = r.try_get("tenant_id")?;
        Ok(tenant_id)
    } else {
        Err(AppError::InvalidApiKey)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observe_places_latency_in_first_matching_bucket() {
        let mut h = TenantLatencyHistogram::new();
        h.observe(3);
        h.observe(7);
        h.observe(50);
        h.observe(5000);

        assert_eq!(h.buckets[0], 1); // <= 5
        assert_eq!(h.buckets[1], 1); // <= 10
        assert_eq!(h.buckets[3], 1); // <= 50
        assert_eq!(h.buckets[TENANT_LATENCY_BUCKETS_MS.len()], 1); // overflow
        assert_eq!(h.count, 4);
        assert_eq!(h.sum_ms, 5060);
    }

    #[test]
    fn low_volume_tenants_roll_into_sentinel_series() {
        let mut store = TenantLatencyStore::new();
        let noisy = Uuid::new_v4();
        let quiet = Uuid::new_v4();

        for _ in 0..TENANT_LATENCY_MIN_OBSERVATIONS {
            store.observe(noisy, TenantLatencyPath::WebhookIngestion, 12);
        }
        store.observe(quiet, TenantLatencyPath::WebhookIngestion, 12);

        let snap = store.snapshot(TenantLatencyPath::WebhookIngestion);
        assert!(snap.iter().any(|(label, _)| label == &noisy.to_string()));
        assert!(snap.iter().any(|(label, _)| label == LOW_VOLUME_TENANT_LABEL));
        assert!(!snap.iter().any(|(label, _)| label == &quiet.to_string()));
    }

    #[test]
    fn cardinality_is_bounded_under_large_multi_tenant_load() {
        let mut store = TenantLatencyStore::new();
        // 10k tenants, each with a single observation: none are reportable,
        // so the emitted series count must stay at 1 (the sentinel) per path.
        for _ in 0..10_000 {
            store.observe(Uuid::new_v4(), TenantLatencyPath::GraphqlQuery, 8);
        }
        let snap = store.snapshot(TenantLatencyPath::GraphqlQuery);
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].0, LOW_VOLUME_TENANT_LABEL);
        assert_eq!(snap[0].1.count, 10_000);
    }
}
