use axum::{
    async_trait,
    extract::{FromRef, FromRequestParts},
    http::{request::Parts, HeaderMap},
};
use serde::{Deserialize, Serialize};
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
