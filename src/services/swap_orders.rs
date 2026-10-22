//! Swap order service layer.
//!
//! Single place for swap order business logic (validation, quoting, cancel
//! rules, exposure aggregation). Transport layers such as GraphQL delegate
//! here and only translate [`AppError`] into their own error format.

use crate::error::AppError;
use chrono::{DateTime, Duration, Utc};
use sqlx::types::BigDecimal;
use sqlx::PgPool;
use uuid::Uuid;

/// Hard cap on page size for order listings.
pub const MAX_PAGE_SIZE: i64 = 100;
/// Page size used when the caller does not specify one.
pub const DEFAULT_PAGE_SIZE: i64 = 20;
/// How long a quote stays valid after order creation.
const QUOTE_TTL_SECS: i64 = 60;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SwapOrderRow {
    pub id: Uuid,
    pub account: String,
    pub source_asset: String,
    pub target_asset: String,
    pub amount: BigDecimal,
    pub status: String,
    pub quote_rate: BigDecimal,
    pub quote_expires_at: DateTime<Utc>,
    pub executed_amount: Option<BigDecimal>,
    pub executed_rate: Option<BigDecimal>,
    pub executed_at: Option<DateTime<Utc>>,
    pub settlement_id: Option<Uuid>,
    pub settled_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ExposureRow {
    pub asset: String,
    pub open_orders: i64,
    pub total_amount: BigDecimal,
}

/// Clamp a caller-supplied page size to `1..=MAX_PAGE_SIZE`.
pub fn clamp_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(DEFAULT_PAGE_SIZE).clamp(1, MAX_PAGE_SIZE)
}

pub async fn get_order(pool: &PgPool, id: Uuid) -> Result<SwapOrderRow, AppError> {
    sqlx::query_as::<_, SwapOrderRow>("SELECT * FROM swap_orders WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::NotFound(format!("swap order {id}")))
}

pub async fn list_orders_by_account(
    pool: &PgPool,
    account: &str,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<SwapOrderRow>, AppError> {
    sqlx::query_as::<_, SwapOrderRow>(
        "SELECT * FROM swap_orders WHERE account = $1 \
         ORDER BY created_at DESC, id LIMIT $2 OFFSET $3",
    )
    .bind(account)
    .bind(clamp_limit(limit))
    .bind(offset.unwrap_or(0).max(0))
    .fetch_all(pool)
    .await
    .map_err(AppError::from)
}

/// Current exposure: open (unexecuted) order volume per source asset.
pub async fn current_exposure(pool: &PgPool) -> Result<Vec<ExposureRow>, AppError> {
    sqlx::query_as::<_, ExposureRow>(
        "SELECT source_asset AS asset, COUNT(*) AS open_orders, \
                COALESCE(SUM(amount), 0) AS total_amount \
         FROM swap_orders WHERE status = 'open' \
         GROUP BY source_asset ORDER BY source_asset",
    )
    .fetch_all(pool)
    .await
    .map_err(AppError::from)
}

pub async fn create_order(
    pool: &PgPool,
    account: &str,
    source_asset: &str,
    target_asset: &str,
    amount: BigDecimal,
    quote_rate: BigDecimal,
) -> Result<SwapOrderRow, AppError> {
    if account.trim().is_empty() {
        return Err(AppError::Validation("account must not be empty".into()));
    }
    if source_asset.is_empty() || target_asset.is_empty() || source_asset == target_asset {
        return Err(AppError::Validation(
            "source and target assets must be set and different".into(),
        ));
    }
    if amount <= BigDecimal::from(0) {
        return Err(AppError::Validation("amount must be positive".into()));
    }
    if quote_rate <= BigDecimal::from(0) {
        return Err(AppError::Validation("quote rate must be positive".into()));
    }

    sqlx::query_as::<_, SwapOrderRow>(
        "INSERT INTO swap_orders \
           (account, source_asset, target_asset, amount, quote_rate, quote_expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING *",
    )
    .bind(account)
    .bind(source_asset)
    .bind(target_asset)
    .bind(amount)
    .bind(quote_rate)
    .bind(Utc::now() + Duration::seconds(QUOTE_TTL_SECS))
    .fetch_one(pool)
    .await
    .map_err(AppError::from)
}

/// Cancel an order. Only `open` orders can be cancelled.
pub async fn cancel_order(pool: &PgPool, id: Uuid) -> Result<SwapOrderRow, AppError> {
    let updated = sqlx::query_as::<_, SwapOrderRow>(
        "UPDATE swap_orders SET status = 'cancelled', updated_at = NOW() \
         WHERE id = $1 AND status = 'open' RETURNING *",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::from)?;

    match updated {
        Some(row) => Ok(row),
        None => {
            // Distinguish "missing" from "not cancellable".
            let existing = get_order(pool, id).await?;
            Err(AppError::BadRequest(format!(
                "swap order {} is {} and cannot be cancelled",
                id, existing.status
            )))
        }
    }
}
