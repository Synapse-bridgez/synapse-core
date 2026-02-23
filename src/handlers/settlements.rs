use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Serialize, ToSchema)]
pub struct SettlementListResponse {
    pub settlements: Vec<crate::schemas::SettlementSchema>,
    pub total: i64,
}

use crate::ApiState;

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct Pagination {
    #[serde(default)]
    pub page: Option<u32>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SettlementListResponse {
    pub settlements: Vec<crate::db::models::Settlement>,
    pub total: i64,
    pub page: u32,
    pub limit: u32,
}

#[utoipa::path(
    get,
    path = "/settlements",
    params(Pagination),
    responses(
        (status = 200, description = "List of settlements", body = SettlementListResponse),
        (status = 500, description = "Database error")
    ),
    tag = "Settlements"
)]
pub async fn list_settlements(
    State(_state): State<ApiState>,
    _query: Query<Pagination>,
) -> Result<Json<SettlementListResponse>, StatusCode> {
    // TODO: Implement settlement listing
    Ok(Json(SettlementListResponse {
        settlements: vec![],
        total: 0,
        page: _query.page.unwrap_or(1),
        limit: _query.limit.unwrap_or(10).min(100),
    }))
}

pub async fn get_settlement(
    State(_state): State<ApiState>,
    Path(_id): Path<String>,
) -> Result<Json<crate::db::models::Settlement>, StatusCode> {
    // TODO: Implement settlement retrieval
    Err(StatusCode::NOT_IMPLEMENTED)
}