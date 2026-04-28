use crate::middleware::quota::{Quota, QuotaManager, QuotaStatus, ResetSchedule, Tier};
use crate::ApiState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json},
};
use crate::error::AppError;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct TenantQuotaView {
    pub tenant_id: Uuid,
    pub name: String,
    pub rate_limit_per_minute: i32,
    pub quota_status: Option<QuotaStatus>,
}

#[derive(Debug, Deserialize)]
pub struct SetQuotaRequest {
    pub custom_limit: Option<u32>,
    pub tier: Option<String>,
}

fn parse_tier(s: &str) -> Tier {
    match s.to_lowercase().as_str() {
        "standard" => Tier::Standard,
        "premium" => Tier::Premium,
        _ => Tier::Free,
    }
}

fn make_manager(redis_url: &str) -> Result<QuotaManager, AppError> {
    QuotaManager::new(redis_url).map_err(|e| AppError::Redis(e))
}

/// GET /admin/quotas — list quota usage for all active tenants.
pub async fn list_tenant_quotas(State(state): State<ApiState>) -> Result<impl IntoResponse, AppError> {
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
    let cfg = state.app_state.get_tenant_config(tenant_id).await
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

/// PUT /admin/quotas/:tenant_id — override quota config for a tenant.
pub async fn set_tenant_quota(
    State(state): State<ApiState>,
    Path(tenant_id): Path<Uuid>,
    Json(payload): Json<SetQuotaRequest>,
) -> Result<impl IntoResponse, AppError> {
    if state.app_state.get_tenant_config(tenant_id).await.is_none() {
        return Err(AppError::NotFound("tenant not found".to_string()));
    }

    let manager = make_manager(&state.app_state.redis_url)?;

    let tier = payload
        .tier
        .as_deref()
        .map(parse_tier)
        .unwrap_or(Tier::Free);

    let quota = Quota {
        tier,
        custom_limit: payload.custom_limit,
        reset_schedule: ResetSchedule::Hourly,
    };

    let key = format!("tenant:{tenant_id}");
    manager.set_quota_config(&key, &quota).await?;
    
    Ok((
        StatusCode::OK,
        Json(serde_json::json!({"message": "quota updated", "tenant_id": tenant_id})),
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
