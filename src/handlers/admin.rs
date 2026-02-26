use crate::middleware::quota::{Quota, QuotaManager, QuotaStatus, ResetSchedule, Tier};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};

pub struct AdminState {
    pub quota_manager: QuotaManager,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SetQuotaRequest {
    pub tier: String,
    pub custom_limit: Option<u32>,
    pub reset_schedule: String,
}

#[derive(Debug, Serialize)]
pub struct QuotaResponse {
    pub key: String,
    pub tier: String,
    pub limit: u32,
    pub used: u32,
    pub remaining: u32,
    pub reset_in_seconds: u64,
}

/// Get quota status for a key
pub async fn get_quota_status(
    State(state): State<AdminState>,
    Path(key): Path<String>,
) -> Result<Json<QuotaStatus>, (StatusCode, String)> {
    state
        .quota_manager
        .check_quota(&key)
        .await
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// Set quota configuration for a key
pub async fn set_quota(
    State(state): State<AdminState>,
    Path(key): Path<String>,
    Json(req): Json<SetQuotaRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    let tier = match req.tier.to_lowercase().as_str() {
        "free" => Tier::Free,
        "standard" => Tier::Standard,
        "premium" => Tier::Premium,
        _ => return Err((StatusCode::BAD_REQUEST, "Invalid tier".to_string())),
    };

    let reset_schedule = match req.reset_schedule.to_lowercase().as_str() {
        "hourly" => ResetSchedule::Hourly,
        "daily" => ResetSchedule::Daily,
        "monthly" => ResetSchedule::Monthly,
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                "Invalid reset schedule".to_string(),
            ))
        }
    };

    let quota = Quota {
        tier,
        custom_limit: req.custom_limit,
        reset_schedule,
    };

    state
        .quota_manager
        .set_quota_config(&key, &quota)
        .await
        .map(|_| StatusCode::OK)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// Reset quota usage for a key
pub async fn reset_quota(
    State(state): State<AdminState>,
    Path(key): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .quota_manager
        .reset_quota(&key)
        .await
        .map(|_| StatusCode::OK)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}
