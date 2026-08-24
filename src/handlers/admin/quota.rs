use crate::error::AppError;
use crate::middleware::quota::{QuotaManager, QuotaStatus};
use crate::ApiState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct TenantQuotaView {
    pub tenant_id: Uuid,
    pub name: String,
    pub rate_limit_per_minute: i32,
    pub quota_status: Option<QuotaStatus>,
}

/// `custom_limit` is the per-minute limit written to
/// `tenants.rate_limit_per_minute` — the value `rate_limit_middleware`
/// actually enforces. There is no more tier-based quota model on the live
/// enforcement path (see `set_tenant_quota`'s doc comment), so no `tier`
/// field is accepted here; a request that still sends one is unaffected
/// since unknown JSON fields are ignored by default.
#[derive(Debug, Deserialize)]
pub struct SetQuotaRequest {
    pub custom_limit: Option<u32>,
}

fn make_manager(redis_url: &str) -> Result<QuotaManager, AppError> {
    QuotaManager::new(redis_url).map_err(AppError::Redis)
}

/// GET /admin/quotas — list quota usage for all active tenants.
pub async fn list_tenant_quotas(
    State(state): State<ApiState>,
) -> Result<impl IntoResponse, AppError> {
    let manager = make_manager(&state.app_state.redis_url)?;

    let configs = state.app_state.tenant_configs.read().await;
    let mut views = Vec::new();

    for (tid, cfg) in configs.iter() {
        let key = format!("tenant:{tid}");
        let quota_status = manager
            .check_quota_with_limit(&key, cfg.rate_limit_per_minute as u32)
            .await
            .ok();

        views.push(TenantQuotaView {
            tenant_id: *tid,
            name: cfg.name.clone(),
            rate_limit_per_minute: cfg.rate_limit_per_minute,
            quota_status,
        });
    }

    Ok((StatusCode::OK, Json(views)))
}

/// GET /admin/quotas/:tenant_id — quota usage for a single tenant.
pub async fn get_tenant_quota(
    State(state): State<ApiState>,
    Path(tenant_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let cfg = state
        .app_state
        .get_tenant_config(tenant_id)
        .await
        .ok_or_else(|| AppError::NotFound("tenant not found".to_string()))?;

    let manager = make_manager(&state.app_state.redis_url)?;

    let key = format!("tenant:{tenant_id}");
    let quota_status = manager
        .check_quota_with_limit(&key, cfg.rate_limit_per_minute as u32)
        .await
        .ok();

    Ok((
        StatusCode::OK,
        Json(TenantQuotaView {
            tenant_id,
            name: cfg.name,
            rate_limit_per_minute: cfg.rate_limit_per_minute,
            quota_status,
        }),
    ))
}

/// PUT /admin/quotas/:tenant_id — override the rate limit actually enforced
/// for a tenant.
///
/// This used to write to Redis key `quota:config:{tenant_id}` via
/// `QuotaManager::set_quota_config` — a config the live rate limiter
/// (`middleware::quota::rate_limit_middleware`) never reads. That middleware
/// computes its limit from `AppState.tenant_configs`, which is loaded from
/// `tenants.rate_limit_per_minute`. An admin calling this endpoint got a 200
/// and reasonably believed the enforced limit changed; it didn't. This now
/// writes to the same column the live path reads, so there is exactly one
/// source of truth: `tenants.rate_limit_per_minute`, read by both admin GET
/// and the enforcement middleware.
///
/// `tier` is accepted for backward request-shape compatibility but is no
/// longer meaningful: the tier-based hourly-limit model
/// (`Tier::requests_per_hour`) was only ever consumed by the dead
/// `get_quota_config`/`check_quota`/`consume_quota` path, which nothing in
/// the live request path calls. `custom_limit` — the per-minute limit — is
/// required; a tier-only request is rejected rather than silently accepted
/// and ignored, which is what the old code effectively did.
pub async fn set_tenant_quota(
    State(state): State<ApiState>,
    Path(tenant_id): Path<Uuid>,
    Json(payload): Json<SetQuotaRequest>,
) -> Result<impl IntoResponse, AppError> {
    if state.app_state.get_tenant_config(tenant_id).await.is_none() {
        return Err(AppError::NotFound("tenant not found".to_string()));
    }

    let new_limit = payload.custom_limit.ok_or_else(|| {
        AppError::BadRequest(
            "custom_limit is required: it is the per-minute limit actually enforced \
             (tenants.rate_limit_per_minute); 'tier' alone no longer maps to anything \
             the live rate limiter reads"
                .to_string(),
        )
    })? as i32;

    let updated_limit = crate::db::queries::update_tenant_rate_limit(
        &state.app_state.db,
        tenant_id,
        new_limit,
        "admin_quota_endpoint",
    )
    .await
    .map_err(|e| match e {
        sqlx::Error::RowNotFound => AppError::NotFound("tenant not found".to_string()),
        sqlx::Error::Decode(msg) => AppError::BadRequest(msg.to_string()),
        other => AppError::DatabaseError(other.to_string()),
    })?;

    // Live enforcement reads AppState.tenant_configs, an in-memory cache
    // that otherwise only refreshes on a 60s background timer (see
    // main.rs's tenant_reload_state task) — reload it inline so the change
    // this endpoint claims to have made is actually enforced on the very
    // next request, not up to a minute later.
    if let Err(e) = state.app_state.load_tenant_configs().await {
        tracing::warn!(
            tenant_id = %tenant_id,
            error = %e,
            "quota updated in the database but in-memory tenant_configs reload failed; \
             the new limit will not be enforced until the next 60s background reload"
        );
    }

    tracing::info!(
        counter.quota_config_updates_total = 1u64,
        tenant_id = %tenant_id,
        new_limit = updated_limit,
        "Tenant quota (rate_limit_per_minute) updated via admin endpoint"
    );

    Ok((
        StatusCode::OK,
        Json(serde_json::json!({
            "message": "quota updated",
            "tenant_id": tenant_id,
            "rate_limit_per_minute": updated_limit,
        })),
    ))
}

/// DELETE /admin/quotas/:tenant_id/reset — reset current usage counter.
pub async fn reset_tenant_quota(
    State(state): State<ApiState>,
    Path(tenant_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let manager = make_manager(&state.app_state.redis_url)?;

    let key = format!("tenant:{tenant_id}");
    manager.reset_quota(&key).await?;

    Ok((
        StatusCode::OK,
        Json(serde_json::json!({"message": "quota reset", "tenant_id": tenant_id})),
    ))
}
