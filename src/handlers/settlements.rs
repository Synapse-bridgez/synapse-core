use crate::utils::cursor as cursor_util;
use crate::ApiState;
use axum::{
    extract::{Path, Query, State},
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct SettlementListQuery {
    pub cursor: Option<String>,
    pub limit: Option<i64>,
    /// "forward" (default) or "backward"
    pub direction: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SettlementListResponse {
    pub settlements: Vec<crate::db::models::Settlement>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

pub async fn list_settlements(
    State(state): State<ApiState>,
    Query(params): Query<SettlementListQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let limit = params.limit.unwrap_or(10).min(100).max(1);
    let backward = params.direction.as_deref() == Some("backward");

    let decoded_cursor = if let Some(ref c) = params.cursor {
        match cursor_util::decode(c) {
            Ok(pair) => Some(pair),
            Err(_) => return Err(StatusCode::BAD_REQUEST),
        }
    } else {
        None
    };

    let fetch_limit = limit + 1;
    let (pool, replica_used) = state.app_state.pool_manager.read_pool().await;
    let mut settlements =
        crate::db::queries::list_settlements_cursor(pool, fetch_limit, decoded_cursor, backward)
            .await
            .map_err(|e| {
                tracing::error!("Failed to list settlements: {:?}", e);
                StatusCode::INTERNAL_SERVER_ERROR
            })?;

    let has_more = settlements.len() as i64 > limit;
    if has_more {
        settlements.truncate(limit as usize);
    }

    let next_cursor = settlements
        .last()
        .map(|s| cursor_util::encode(s.created_at, s.id));

    let body = SettlementListResponse {
        settlements,
        next_cursor,
        has_more,
    };

    let mut response: Response = Json(body).into_response();
    if replica_used {
        response
            .headers_mut()
            .insert("X-Read-Consistency", HeaderValue::from_static("eventual"));
    }

    Ok(response)
}

pub async fn get_settlement(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, StatusCode> {
    let (pool, replica_used) = state.app_state.pool_manager.read_pool().await;
    let settlement = crate::db::queries::get_settlement(pool, id)
        .await
        .map_err(|e| {
            if matches!(e, sqlx::Error::RowNotFound) {
                StatusCode::NOT_FOUND
            } else {
                tracing::error!("Failed to get settlement: {:?}", e);
                StatusCode::INTERNAL_SERVER_ERROR
            }
        })?;

    let mut response: Response = Json(settlement).into_response();
    if replica_used {
        response
            .headers_mut()
            .insert("X-Read-Consistency", HeaderValue::from_static("eventual"));
    }

    Ok(response)
}

/// Request body for admin settlement status changes.
#[derive(Debug, Deserialize)]
pub struct UpdateSettlementStatusRequest {
    pub status: String,
    pub reason: Option<String>,
    /// New total amount — only meaningful when transitioning to "adjusted".
    pub new_total: Option<String>,
    /// Actor performing the change (defaults to "admin").
    pub actor: Option<String>,
}

/// PATCH /admin/settlements/:id/status
/// Allowed transitions: completed→pending_review, →disputed, pending_review→adjusted/voided/disputed,
/// disputed→adjusted/voided/pending_review.
pub async fn update_settlement_status(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateSettlementStatusRequest>,
) -> impl IntoResponse {
    let new_total: Option<sqlx::types::BigDecimal> = match payload.new_total.as_deref() {
        Some(s) => match s.parse() {
            Ok(v) => Some(v),
            Err(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": "invalid new_total"})),
                )
                    .into_response()
            }
        },
        None => None,
    };

    let actor = payload.actor.as_deref().unwrap_or("admin");
    let service = crate::services::SettlementService::new(state.app_state.db.clone());

    match service
        .update_status(id, &payload.status, payload.reason.as_deref(), new_total.as_ref(), actor)
        .await
    {
        Ok(settlement) => (StatusCode::OK, Json(settlement)).into_response(),
        Err(crate::error::AppError::NotFound(msg)) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": msg})),
        )
            .into_response(),
        Err(crate::error::AppError::BadRequest(msg)) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": msg})),
        )
            .into_response(),
        Err(e) => {
            tracing::error!("Failed to update settlement status {}: {:?}", id, e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "internal error"})),
            )
                .into_response()
        }
    }
}
