use crate::middleware::quota::{Quota, QuotaManager, QuotaStatus, ResetSchedule, Tier};
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

fn make_manager(redis_url: &str) -> Result<QuotaManager, String> {
    QuotaManager::new(redis_url).map_err(|e| format!("Redis unavailable: {e}"))
}

/// GET /admin/quotas — list quota usage for all active tenants.
pub async fn list_tenant_quotas(State(state): State<ApiState>) -> impl IntoResponse {
    let manager = match make_manager(&state.app_state.redis_url) {
        Ok(m) => m,
        Err(e) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": e})),
            )
                .into_response()
        }
    };

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

    (StatusCode::OK, Json(views)).into_response()
}

/// GET /admin/quotas/:tenant_id — quota usage for a single tenant.
pub async fn get_tenant_quota(
    State(state): State<ApiState>,
    Path(tenant_id): Path<Uuid>,
) -> impl IntoResponse {
    let cfg = match state.app_state.get_tenant_config(tenant_id).await {
        Some(c) => c,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "tenant not found"})),
            )
                .into_response()
        }
    };

    let manager = match make_manager(&state.app_state.redis_url) {
        Ok(m) => m,
        Err(e) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": e})),
            )
                .into_response()
        }
    };

    let key = format!("tenant:{tenant_id}");
    let quota_status = manager
        .check_quota_with_limit(&key, cfg.rate_limit_per_minute as u32)
        .await
        .ok();

    (
        StatusCode::OK,
        Json(TenantQuotaView {
            tenant_id,
            name: cfg.name,
            rate_limit_per_minute: cfg.rate_limit_per_minute,
            quota_status,
        }),
    )
        .into_response()
}

/// PUT /admin/quotas/:tenant_id — override quota config for a tenant.
pub async fn set_tenant_quota(
    State(state): State<ApiState>,
    Path(tenant_id): Path<Uuid>,
    Json(payload): Json<SetQuotaRequest>,
) -> impl IntoResponse {
    if state.app_state.get_tenant_config(tenant_id).await.is_none() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "tenant not found"})),
        )
            .into_response();
    }

    let manager = match make_manager(&state.app_state.redis_url) {
        Ok(m) => m,
        Err(e) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": e})),
            )
                .into_response()
        }
    };

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
    match manager.set_quota_config(&key, &quota).await {
        Ok(_) => (
            StatusCode::OK,
            Json(serde_json::json!({"message": "quota updated", "tenant_id": tenant_id})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// DELETE /admin/quotas/:tenant_id/reset — reset current usage counter.
pub async fn reset_tenant_quota(
    State(state): State<ApiState>,
    Path(tenant_id): Path<Uuid>,
) -> impl IntoResponse {
    let manager = match make_manager(&state.app_state.redis_url) {
        Ok(m) => m,
        Err(e) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error": e})),
            )
                .into_response()
        }
    };

    let key = format!("tenant:{tenant_id}");
    match manager.reset_quota(&key).await {
        Ok(_) => (
            StatusCode::OK,
            Json(serde_json::json!({"message": "quota reset", "tenant_id": tenant_id})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}
