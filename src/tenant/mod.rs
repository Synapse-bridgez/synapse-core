use axum::{
    async_trait,
    extract::{FromRef, FromRequestParts},
    http::{request::Parts, HeaderMap},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{error::AppError, AppState};

/// Longest tenant id accepted from a request header.
pub const TENANT_ID_MAX_LEN: usize = 128;

/// A tenant identifier taken from a request header such as `X-Tenant-Id`.
///
/// Parsing rules (also described in `docs/tenant-rls-request-path.md`):
/// surrounding whitespace is trimmed, the result must not be empty, must be at
/// most [`TENANT_ID_MAX_LEN`] characters, and must not contain whitespace or
/// control characters. This only checks the shape of the value; it does not
/// prove that the tenant exists or that the caller may act for it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TenantId(String);

impl TenantId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TenantId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Error returned when a string is not a valid [`TenantId`]. It names the
/// offending input so logs show what was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid tenant id {input:?}: {reason}")]
pub struct ParseTenantIdError {
    pub input: String,
    pub reason: &'static str,
}

impl std::str::FromStr for TenantId {
    type Err = ParseTenantIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let trimmed = s.trim();
        let reject = |reason: &'static str| ParseTenantIdError {
            input: s.to_string(),
            reason,
        };
        if trimmed.is_empty() {
            return Err(reject("must not be empty"));
        }
        if trimmed.chars().count() > TENANT_ID_MAX_LEN {
            return Err(reject("is too long"));
        }
        if trimmed.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err(reject("must not contain whitespace or control characters"));
        }
        Ok(TenantId(trimmed.to_string()))
    }
}

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
mod tenant_id_tests {
    use super::*;

    #[test]
    fn parses_and_trims_a_valid_id() {
        let id: TenantId = "  tenant-42 ".parse().unwrap();
        assert_eq!(id.as_str(), "tenant-42");
        assert_eq!(id.to_string(), "tenant-42");
    }

    #[test]
    fn accepts_a_uuid() {
        let raw = "550e8400-e29b-41d4-a716-446655440000";
        assert_eq!(raw.parse::<TenantId>().unwrap().as_str(), raw);
    }

    #[test]
    fn rejects_empty_and_blank_input() {
        assert!("".parse::<TenantId>().is_err());
        assert!("   ".parse::<TenantId>().is_err());
    }

    #[test]
    fn rejects_inner_whitespace_and_control_characters() {
        assert!("a b".parse::<TenantId>().is_err());
        assert!("a\u{7}b".parse::<TenantId>().is_err());
    }

    #[test]
    fn rejects_overlong_ids() {
        let long = "a".repeat(TENANT_ID_MAX_LEN + 1);
        assert!(long.parse::<TenantId>().is_err());
        assert!("a".repeat(TENANT_ID_MAX_LEN).parse::<TenantId>().is_ok());
    }

    #[test]
    fn error_names_the_offending_input() {
        let err = "bad id".parse::<TenantId>().unwrap_err();
        assert_eq!(err.input, "bad id");
        assert!(err.to_string().contains("bad id"));
    }
}
