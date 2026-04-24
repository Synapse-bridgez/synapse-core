use crate::ApiState;
use axum::{
    extract::{Path, Query, State},
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde::{Deserialize, Serialize};
use utoipa::IntoParams;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, ToSchema, IntoParams)]
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
    State(state): State<ApiState>,
    query: Query<Pagination>,
) -> Result<impl IntoResponse, StatusCode> {
    let page = query.page.unwrap_or(1).max(1);
    let limit = query.limit.unwrap_or(10).min(100).max(1);
    let offset = (page.saturating_sub(1) as i64) * (limit as i64);

    let (pool, replica_used) = state.app_state.pool_manager.read_pool().await;
    let settlements = crate::db::queries::list_settlements(pool, limit as i64, offset)
        .await
        .map_err(|e| {
            tracing::error!("Failed to list settlements: {:?}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let response_body = SettlementListResponse {
        settlements,
        total: settlements.len() as i64,
        page,
        limit,
    };

    let mut response: Response = Json(response_body).into_response();
    if replica_used {
        response.headers_mut().insert(
            "X-Read-Consistency",
            HeaderValue::from_static("eventual"),
        );
    }

    Ok(response)
}

#[utoipa::path(
    get,
    path = "/settlements/{id}",
    params(
        ("id" = String, Path, description = "Settlement ID")
    ),
    responses(
        (status = 200, description = "Settlement found", body = crate::db::models::Settlement),
        (status = 404, description = "Settlement not found"),
        (status = 501, description = "Not implemented")
    ),
    tag = "Settlements"
)]
pub async fn get_settlement(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, StatusCode> {
    let (pool, replica_used) = state.app_state.pool_manager.read_pool().await;
    let settlement = crate::db::queries::get_settlement(pool, id).await.map_err(|e| {
        if matches!(e, sqlx::Error::RowNotFound) {
            StatusCode::NOT_FOUND
        } else {
            tracing::error!("Failed to get settlement: {:?}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        }
    })?;

    let mut response: Response = Json(settlement).into_response();
    if replica_used {
        response.headers_mut().insert(
            "X-Read-Consistency",
            HeaderValue::from_static("eventual"),
        );
    }

    Ok(response)
}
