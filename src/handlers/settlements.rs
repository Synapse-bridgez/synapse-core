use axum::{
    extract::{Path, State, Query},
    response::IntoResponse,
    Json,
};
use uuid::Uuid;
use crate::db::queries;
use crate::error::AppError;
use serde::{Deserialize};
use utoipa::{ToSchema, IntoParams};
use bigdecimal::BigDecimal as ExternalBigDecimal;
use std::str::FromStr;

#[derive(Deserialize, ToSchema, IntoParams)]
pub struct Pagination {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

use crate::AppState;
use crate::schemas::SettlementListResponse;

pub async fn list_settlements(
    State(state): State<AppState>,
    Query(pagination): Query<Pagination>,
) -> Result<impl IntoResponse, AppError> {
    let limit = pagination.limit.unwrap_or(20);
    let offset = pagination.offset.unwrap_or(0);

    let settlements = queries::list_settlements(&state.db, limit, offset).await?;

    let settlement_schemas = settlements
        .into_iter()
        .map(|s| crate::schemas::SettlementSchema {
            id: s.id,
            asset_code: s.asset_code,
            total_amount: ExternalBigDecimal::from_str(&s.total_amount.to_string()).unwrap_or_default(),
            transaction_count: s.transaction_count as i64,
            status: s.status,
            created_at: s.created_at,
            updated_at: s.updated_at,
        })
        .collect();

    Ok(Json(SettlementListResponse {
        settlements: settlement_schemas,
        total: limit,
        page: 1,
        per_page: limit as i32,
    }))
}

/// Get a settlement by ID
/// 
/// Returns details for a specific settlement
#[utoipa::path(
    get,
    path = "/settlements/{id}",
    params(
        ("id" = String, Path, description = "Settlement ID")
    ),
    responses(
        (status = 200, description = "Settlement found", body = crate::schemas::SettlementSchema),
        (status = 404, description = "Settlement not found"),
        (status = 500, description = "Database error")
    ),
    tag = "Settlements"
)]
pub async fn get_settlement(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let settlement = queries::get_settlement(&state.db, id).await
        .map_err(|e| match e {
            sqlx::Error::RowNotFound => AppError::NotFound(format!("Settlement {} not found", id)),
            _ => AppError::Database(e),
        })?;

    Ok(Json(settlement))
}
