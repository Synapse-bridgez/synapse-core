use axum::{
    extract::{Path, State, Query},
    response::IntoResponse,
    Json,
};
use uuid::Uuid;
use crate::db::queries;
use crate::error::AppError;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct Pagination {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

use crate::ApiState;

pub async fn list_settlements(
    State(state): State<ApiState>,
    Query(pagination): Query<Pagination>,
) -> Result<impl IntoResponse, AppError> {
    let limit = pagination.limit.unwrap_or(20);
    let offset = pagination.offset.unwrap_or(0);

    let settlements = queries::list_settlements(&state.app_state.db, limit, offset).await
        .map_err(|e| AppError::DatabaseError(e.to_string()))?;

    Ok(Json(settlements))
}

pub async fn get_settlement(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let settlement = queries::get_settlement(&state.app_state.db, id).await
        .map_err(|e| match e {
            sqlx::Error::RowNotFound => AppError::NotFound(format!("Settlement {} not found", id)),
            _ => AppError::DatabaseError(e.to_string()),
        })?;

    Ok(Json(settlement))
}
