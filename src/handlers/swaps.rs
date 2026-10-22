//! REST handlers for swap orders (`/swaps`).
//!
//! Thin wrappers over [`crate::swap::orders`], which owns validation, deposit
//! eligibility, idempotency, and the cancellation rule.

use crate::error::AppError;
use crate::handlers::pagination::{
    ApiPaginationConfig, PaginatedListResponse, PaginationHelper, PaginationQuery,
};
use crate::swap::orders::{self, CreateSwapOrder};
use crate::tenant::TenantContext;
use crate::ApiState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use uuid::Uuid;

/// Parse a swap order id from the path, reporting malformed ids in the
/// standard error envelope rather than axum's plain-text rejection.
fn parse_swap_id(raw: &str) -> Result<Uuid, AppError> {
    Uuid::parse_str(raw)
        .map_err(|_| AppError::BadRequest(format!("invalid swap order id '{raw}'")))
}

/// Create a swap order from a completed deposit or an explicit request.
///
/// Returns `201 Created` for a new order. Returns `200 OK` with the original
/// order when `idempotency_key` is replayed with an identical request.
#[tracing::instrument(skip(state, tenant, req))]
pub async fn create_swap_order(
    State(state): State<ApiState>,
    tenant: TenantContext,
    Json(req): Json<CreateSwapOrder>,
) -> Result<impl IntoResponse, AppError> {
    let (order, created) = orders::create_order(&state.app_state.db, tenant.tenant_id, req).await?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(order)))
}

/// Fetch a swap order's current status, quote, and execution details.
pub async fn get_swap_order(
    State(state): State<ApiState>,
    tenant: TenantContext,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let id = parse_swap_id(&id)?;
    let order = orders::get_order(&state.app_state.db, id, tenant.tenant_id).await?;
    Ok(Json(order))
}

/// List the calling tenant's swap orders, newest first, paginated.
pub async fn list_swap_orders(
    State(state): State<ApiState>,
    tenant: TenantContext,
    Query(params): Query<PaginationQuery>,
) -> Result<impl IntoResponse, AppError> {
    let page = PaginationHelper::from_query(&params, &ApiPaginationConfig::default())
        .map_err(|(_, message)| AppError::BadRequest(message))?;
    let (data, total) = orders::list_orders(&state.app_state.db, tenant.tenant_id, &page).await?;
    Ok(Json(PaginatedListResponse::new(
        data,
        total,
        page.page(),
        page.page_size(),
    )))
}

/// Cancel a swap order. Only `pending` orders can be cancelled; any other state
/// returns `409 Conflict` with `ERR_SWAP_001`.
pub async fn cancel_swap_order(
    State(state): State<ApiState>,
    tenant: TenantContext,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let id = parse_swap_id(&id)?;
    let order = orders::cancel_order(&state.app_state.db, id, tenant.tenant_id).await?;
    Ok(Json(order))
}
