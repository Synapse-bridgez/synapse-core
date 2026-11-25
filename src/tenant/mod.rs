use axum::{
    async_trait,
    extract::{FromRef, FromRequestParts},
    http::{request::Parts, HeaderMap},
};
use lru::LruCache;
use serde::{Deserialize, Serialize};
use std::num::NonZeroUsize;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use uuid::Uuid;

use crate::{error::AppError, AppState};

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct TenantConfig {
    pub tenant_id: Uuid,
    pub name: String,
    pub webhook_secret: String,
    pub stellar_account: String,
    pub rate_limit_per_minute: i32,
    pub is_active: bool,
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

/// Cached entry for the api-key → tenant_id mapping.
struct CachedTenantId {
    tenant_id: Uuid,
    inserted_at: Instant,
}

/// Short-lived in-process cache that maps an API-key to its resolved tenant_id.
///
/// This avoids a database round-trip on every request for the same tenant.
/// The cache is bounded (at most 4 096 entries) so it cannot grow without
/// limit, and each entry expires after `TTL` so a deleted or disabled tenant
/// stops being served within that window.
struct TenantIdCache {
    inner: Mutex<LruCache<String, CachedTenantId>>,
    ttl: Duration,
}

impl TenantIdCache {
    fn new(capacity: usize, ttl: Duration) -> Self {
        Self {
            inner: Mutex::new(LruCache::new(
                NonZeroUsize::new(capacity).expect("capacity must be > 0"),
            )),
            ttl,
        }
    }

    fn get(&self, api_key: &str) -> Option<Uuid> {
        let mut cache = self.inner.lock().expect("tenant id cache lock poisoned");
        let entry = cache.get(api_key)?;
        if entry.inserted_at.elapsed() > self.ttl {
            cache.pop(api_key);
            return None;
        }
        Some(entry.tenant_id)
    }

    fn insert(&self, api_key: &str, tenant_id: Uuid) {
        let mut cache = self.inner.lock().expect("tenant id cache lock poisoned");
        cache.put(
            api_key.to_owned(),
            CachedTenantId {
                tenant_id,
                inserted_at: Instant::now(),
            },
        );
    }

    fn invalidate(&self, api_key: &str) {
        let mut cache = self.inner.lock().expect("tenant id cache lock poisoned");
        cache.pop(api_key);
    }
}

/// 5-minute TTL, 4096-entry bounded LRU cache for api-key → tenant_id lookups.
static TENANT_ID_CACHE: std::sync::OnceLock<TenantIdCache> = std::sync::OnceLock::new();

fn tenant_id_cache() -> &'static TenantIdCache {
    TENANT_ID_CACHE.get_or_init(|| TenantIdCache::new(4096, Duration::from_secs(300)))
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
            // Invalidate the cache entry so the tenant is not served again from
            // cache after being disabled.
            if let Some(api_key) = extract_api_key(&parts.headers) {
                tenant_id_cache().invalidate(&api_key);
            }
            return Err(AppError::Unauthorized("tenant inactive".to_string()));
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
        // Fast path: return the cached tenant_id if still fresh.
        if let Some(cached_id) = tenant_id_cache().get(&api_key) {
            return Ok(cached_id);
        }

        // Only failed lookups count against the brute-force budget — see
        // middleware::auth::admin_auth's doc comment for why: a valid key
        // used many times is routine API traffic (already throttled
        // separately and much more generously by
        // middleware::quota::rate_limit_middleware), not a guessing attack.
        match resolve_tenant_by_api_key(&state.db, &api_key).await {
            Ok(tenant_id) => {
                tenant_id_cache().insert(&api_key, tenant_id);
                return Ok(tenant_id);
            }
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
    let row = sqlx::query("SELECT tenant_id FROM tenants WHERE api_key_hash = $1")
        .bind(crate::db::queries::hash_api_key(api_key))
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
    use std::time::Duration;

    #[test]
    fn tenant_id_cache_miss_on_empty() {
        let cache = TenantIdCache::new(16, Duration::from_secs(300));
        assert!(cache.get("unknown-key").is_none());
    }

    #[test]
    fn tenant_id_cache_hit_after_insert() {
        let cache = TenantIdCache::new(16, Duration::from_secs(300));
        let id = Uuid::new_v4();
        cache.insert("key-abc", id);
        assert_eq!(cache.get("key-abc"), Some(id));
    }

    #[test]
    fn tenant_id_cache_expires_after_ttl() {
        let cache = TenantIdCache::new(16, Duration::from_millis(1));
        let id = Uuid::new_v4();
        cache.insert("key-xyz", id);
        std::thread::sleep(Duration::from_millis(5));
        assert!(cache.get("key-xyz").is_none());
    }

    #[test]
    fn tenant_id_cache_invalidate_removes_entry() {
        let cache = TenantIdCache::new(16, Duration::from_secs(300));
        let id = Uuid::new_v4();
        cache.insert("key-del", id);
        cache.invalidate("key-del");
        assert!(cache.get("key-del").is_none());
    }

    #[test]
    fn tenant_id_cache_bounded_by_capacity() {
        let cap = 4;
        let cache = TenantIdCache::new(cap, Duration::from_secs(300));
        for i in 0..=cap {
            cache.insert(&format!("key-{i}"), Uuid::new_v4());
        }
        // LRU eviction: the cache must not exceed capacity
        let count = {
            let inner = cache.inner.lock().unwrap();
            inner.len()
        };
        assert_eq!(count, cap);
    }
}
