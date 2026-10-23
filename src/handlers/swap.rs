//! Swap REST API (#1440): quotes, orders, order history and cancellation.
//!
//! Handlers stay thin: extract the caller (tenant + optional idempotency key),
//! validate, call the domain service in `crate::swap`, map errors through
//! `AppError`. Response schemas are dedicated DTOs so internal columns
//! (tenant id, idempotency key, request hash) never reach clients.
//!
//! Versioning: these routes are part of `data_routes`, so they are served
//! under `/api/v1` and `/api/v2`. Changes within a version are additive only.
//!
//! Also hosts tenant analytics (#1506) and operator surveillance / archive
//! endpoints (#1505, #1508), routed via `swap::versioning`.

use crate::error::AppError;
use crate::swap::analytics::Granularity;
use crate::swap::execution::{self, SwapOrder, SwapOrderStatus};
use crate::swap::market_data::{self, PriceQuery, QuoteQuery};
use crate::swap::quote::{self, SwapQuote};
use crate::tenant::TenantContext;
use crate::utils::cursor as cursor_util;
use crate::ApiState;
use axum::{
    async_trait,
    extract::{FromRequestParts, Path, Query, State},
    http::{request::Parts, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Maximum request body accepted by swap endpoints.
pub const SWAP_MAX_BODY_BYTES: usize = 4 * 1024;
const IDEMPOTENCY_HEADER: &str = "idempotency-key";

/// Shared extractor: resolves the tenant (auth, activity and rate limiting
/// are enforced by `TenantContext`) and the optional `Idempotency-Key`
/// header, so every swap endpoint reads them the same way.
pub struct SwapCaller {
    pub tenant_id: Uuid,
    idempotency_key: Option<String>,
}

impl SwapCaller {
    fn require_idempotency_key(&self) -> Result<&str, AppError> {
        self.idempotency_key
            .as_deref()
            .ok_or_else(|| AppError::Validation("Idempotency-Key header is required".to_string()))
    }
}

#[async_trait]
impl FromRequestParts<ApiState> for SwapCaller {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &ApiState,
    ) -> Result<Self, Self::Rejection> {
        let tenant = TenantContext::from_request_parts(parts, state).await?;
        let idempotency_key = match parts.headers.get(IDEMPOTENCY_HEADER) {
            None => None,
            Some(v) => Some(
                v.to_str()
                    .map_err(|_| AppError::Validation("Idempotency-Key is not valid".to_string()))?
                    .to_string(),
            ),
        };
        Ok(Self {
            tenant_id: tenant.tenant_id,
            idempotency_key,
        })
    }
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateQuoteRequest {
    pub source_asset: String,
    pub dest_asset: String,
    /// Decimal string, at most 7 fractional digits.
    pub source_amount: String,
}

#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateOrderRequest {
    pub quote_id: Uuid,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct QuoteResponse {
    pub quote_id: Uuid,
    pub source_asset: String,
    pub dest_asset: String,
    pub source_amount: String,
    pub dest_amount: String,
    pub rate: String,
    pub fee_bps: i32,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

impl From<SwapQuote> for QuoteResponse {
    fn from(q: SwapQuote) -> Self {
        Self {
            quote_id: q.id,
            source_asset: q.source_asset,
            dest_asset: q.dest_asset,
            source_amount: q.source_amount.to_string(),
            dest_amount: q.dest_amount.to_string(),
            rate: q.rate.normalized().to_string(),
            fee_bps: q.fee_bps,
            expires_at: q.expires_at,
            created_at: q.created_at,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct OrderResponse {
    pub order_id: Uuid,
    pub quote_id: Uuid,
    pub source_asset: String,
    pub dest_asset: String,
    pub source_amount: String,
    pub dest_amount: String,
    /// One of `pending`, `executing`, `completed`, `failed`, `canceled`.
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<SwapOrder> for OrderResponse {
    fn from(o: SwapOrder) -> Self {
        Self {
            order_id: o.id,
            quote_id: o.quote_id,
            source_asset: o.source_asset,
            dest_asset: o.dest_asset,
            source_amount: o.source_amount.to_string(),
            dest_amount: o.dest_amount.to_string(),
            status: o.status,
            created_at: o.created_at,
            updated_at: o.updated_at,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct OrderListResponse {
    pub orders: Vec<OrderResponse>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

#[derive(Debug, Deserialize)]
pub struct OrderListQuery {
    pub status: Option<String>,
    pub created_from: Option<DateTime<Utc>>,
    pub created_to: Option<DateTime<Utc>>,
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

#[utoipa::path(
    post,
    path = "/swap/quotes",
    request_body = CreateQuoteRequest,
    responses(
        (status = 201, description = "Quote created", body = QuoteResponse),
        (status = 400, description = "Validation error"),
        (status = 422, description = "Rejected by risk policy"),
        (status = 429, description = "Rate limit exceeded"),
    ),
    tag = "Swap"
)]
pub async fn create_quote(
    State(state): State<ApiState>,
    caller: SwapCaller,
    Json(req): Json<CreateQuoteRequest>,
) -> Result<impl IntoResponse, AppError> {
    let quote = quote::create_quote(
        &state.app_state.db,
        caller.tenant_id,
        &req.source_asset,
        &req.dest_asset,
        &req.source_amount,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(QuoteResponse::from(quote))))
}

#[utoipa::path(
    post,
    path = "/swap/orders",
    request_body = CreateOrderRequest,
    params(("Idempotency-Key" = String, Header, description = "Client-chosen key, 1-128 chars")),
    responses(
        (status = 201, description = "Order created (or identical replay)", body = OrderResponse),
        (status = 400, description = "Validation error / missing Idempotency-Key"),
        (status = 404, description = "Unknown quote"),
        (status = 409, description = "Quote expired or idempotency key conflict"),
        (status = 422, description = "Rejected by risk policy"),
        (status = 429, description = "Rate limit exceeded"),
    ),
    tag = "Swap"
)]
pub async fn create_order(
    State(state): State<ApiState>,
    caller: SwapCaller,
    Json(req): Json<CreateOrderRequest>,
) -> Result<impl IntoResponse, AppError> {
    let key = caller.require_idempotency_key()?;
    let order =
        execution::create_order(&state.app_state.db, caller.tenant_id, key, req.quote_id).await?;
    Ok((StatusCode::CREATED, Json(OrderResponse::from(order))))
}

#[utoipa::path(
    get,
    path = "/swap/orders/{id}",
    params(("id" = Uuid, Path, description = "Order ID")),
    responses(
        (status = 200, description = "Order", body = OrderResponse),
        (status = 404, description = "Order not found"),
    ),
    tag = "Swap"
)]
pub async fn get_order(
    State(state): State<ApiState>,
    caller: SwapCaller,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let (pool, replica_used) = state.app_state.pool_manager.read_pool().await;
    let order = crate::db::queries::get_swap_order_for_tenant(&pool, id, caller.tenant_id)
        .await?
        .ok_or_else(|| AppError::NotFound("Order not found".to_string()))?;
    let mut response: Response = Json(OrderResponse::from(order)).into_response();
    if replica_used {
        response
            .headers_mut()
            .insert("X-Read-Consistency", HeaderValue::from_static("eventual"));
    }
    Ok(response)
}

#[utoipa::path(
    get,
    path = "/swap/orders",
    params(
        ("status" = Option<String>, Query, description = "pending|executing|completed|failed|canceled"),
        ("created_from" = Option<String>, Query, description = "RFC 3339, inclusive"),
        ("created_to" = Option<String>, Query, description = "RFC 3339, exclusive"),
        ("cursor" = Option<String>, Query, description = "Opaque cursor from a previous page"),
        ("limit" = Option<i64>, Query, description = "Page size (1-100, default 20)"),
    ),
    responses(
        (status = 200, description = "Page of orders, newest first", body = OrderListResponse),
        (status = 400, description = "Invalid filter or cursor"),
    ),
    tag = "Swap"
)]
pub async fn list_orders(
    State(state): State<ApiState>,
    caller: SwapCaller,
    Query(params): Query<OrderListQuery>,
) -> Result<impl IntoResponse, AppError> {
    let limit = params.limit.unwrap_or(20).clamp(1, 100);
    let status = match params.status.as_deref() {
        Some(s) => Some(
            SwapOrderStatus::parse(s)
                .ok_or_else(|| AppError::Validation("unknown status filter".to_string()))?
                .as_str(),
        ),
        None => None,
    };
    if let (Some(from), Some(to)) = (params.created_from, params.created_to) {
        if from >= to {
            return Err(AppError::Validation(
                "created_from must be before created_to".to_string(),
            ));
        }
    }
    let cursor = match params.cursor.as_deref() {
        Some(c) => Some(
            cursor_util::decode(c)
                .map_err(|e| AppError::BadRequest(format!("invalid cursor: {e}")))?,
        ),
        None => None,
    };

    let (pool, replica_used) = state.app_state.pool_manager.read_pool().await;
    let mut orders = crate::db::queries::list_swap_orders_for_tenant(
        &pool,
        caller.tenant_id,
        status,
        params.created_from,
        params.created_to,
        cursor,
        limit + 1,
    )
    .await?;

    let has_more = orders.len() as i64 > limit;
    if has_more {
        orders.truncate(limit as usize);
    }
    let next_cursor = if has_more {
        orders.last().map(|o| cursor_util::encode(o.created_at, o.id))
    } else {
        None
    };
    let body = OrderListResponse {
        orders: orders.into_iter().map(OrderResponse::from).collect(),
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

#[utoipa::path(
    post,
    path = "/swap/orders/{id}/cancel",
    params(("id" = Uuid, Path, description = "Order ID")),
    responses(
        (status = 200, description = "Order canceled", body = OrderResponse),
        (status = 404, description = "Order not found"),
        (status = 409, description = "Order is not cancelable"),
    ),
    tag = "Swap"
)]
pub async fn cancel_order(
    State(state): State<ApiState>,
    caller: SwapCaller,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let order = execution::cancel_order(&state.app_state.db, caller.tenant_id, id).await?;
    Ok(Json(OrderResponse::from(order)))
}
