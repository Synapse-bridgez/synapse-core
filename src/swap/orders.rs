//! Swap order lifecycle for the REST surface (`/swaps`).
//!
//! Handlers stay thin and delegate here: request validation, deposit
//! eligibility, idempotent create, and the cancellation rule all live in this
//! service layer so REST, GraphQL, and CLI can share one code path.
//!
//! Lifecycle: `pending` -> `routed` -> `completed` | `failed`, with
//! `pending` -> `cancelled` allowed. Only the Phase 2 engine moves an order
//! past `pending`; until it exists, orders stay `pending` or `cancelled`.

use crate::db::models::SwapOrder;
use crate::db::queries;
use crate::error::AppError;
use crate::handlers::pagination::PaginationHelper;
use serde::Deserialize;
use sqlx::types::BigDecimal;
use sqlx::PgPool;
use std::str::FromStr;
use uuid::Uuid;

/// Longest accepted client-supplied idempotency key.
pub const MAX_IDEMPOTENCY_KEY_LEN: usize = 128;

/// Initial status of every newly created order.
pub const STATUS_PENDING: &str = "pending";

/// Deposit transaction status required before it can back a swap.
const ELIGIBLE_DEPOSIT_STATUS: &str = "completed";

/// Body of `POST /swaps`. Either `deposit_transaction_id` (asset and amount are
/// taken from the deposit) or explicit `source_asset` + `amount` must be given.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateSwapOrder {
    /// Client-chosen key; retrying with the same key and body returns the original order.
    pub idempotency_key: String,
    /// Completed deposit transaction to swap from.
    pub deposit_transaction_id: Option<Uuid>,
    /// Asset to sell. Required without a deposit; must match the deposit otherwise.
    pub source_asset: Option<String>,
    /// Asset to buy.
    pub target_asset: String,
    /// Decimal amount of `source_asset`. Required without a deposit; must match otherwise.
    pub amount: Option<String>,
}

/// Create a swap order for `tenant_id`. Returns the order and whether it was
/// newly created (`false` means an identical earlier request was replayed).
pub async fn create_order(
    pool: &PgPool,
    tenant_id: Uuid,
    req: CreateSwapOrder,
) -> Result<(SwapOrder, bool), AppError> {
    let idempotency_key = req.idempotency_key.trim().to_string();
    if idempotency_key.is_empty() || idempotency_key.len() > MAX_IDEMPOTENCY_KEY_LEN {
        return Err(AppError::Validation(format!(
            "idempotency_key must be 1-{MAX_IDEMPOTENCY_KEY_LEN} characters"
        )));
    }
    let target_asset = validate_asset_code("target_asset", &req.target_asset)?;
    let requested_amount = req.amount.as_deref().map(parse_amount).transpose()?;

    let (source_asset, amount) = match req.deposit_transaction_id {
        Some(deposit_id) => {
            let deposit = queries::get_transaction_for_tenant(pool, deposit_id, tenant_id)
                .await
                .map_err(|e| match e {
                    sqlx::Error::RowNotFound => {
                        AppError::NotFound(format!("Deposit transaction {deposit_id} not found"))
                    }
                    other => other.into(),
                })?;
            if deposit.status != ELIGIBLE_DEPOSIT_STATUS {
                return Err(AppError::SwapDepositIneligible(format!(
                    "deposit transaction is '{}', expected '{ELIGIBLE_DEPOSIT_STATUS}'",
                    deposit.status
                )));
            }
            if let Some(source) = &req.source_asset {
                if source != &deposit.asset_code {
                    return Err(AppError::Validation(
                        "source_asset must match the deposit transaction's asset".to_string(),
                    ));
                }
            }
            if requested_amount.as_ref().is_some_and(|a| a != &deposit.amount) {
                return Err(AppError::Validation(
                    "amount must match the deposit transaction's amount".to_string(),
                ));
            }
            (deposit.asset_code, deposit.amount)
        }
        None => {
            let source = req.source_asset.as_deref().ok_or_else(|| {
                AppError::Validation("source_asset is required without a deposit".to_string())
            })?;
            let amount = requested_amount.ok_or_else(|| {
                AppError::Validation("amount is required without a deposit".to_string())
            })?;
            (validate_asset_code("source_asset", source)?, amount)
        }
    };

    if source_asset == target_asset {
        return Err(AppError::Validation(
            "source_asset and target_asset must differ".to_string(),
        ));
    }
    if amount <= BigDecimal::from(0) {
        return Err(AppError::Validation("amount must be positive".to_string()));
    }

    let deposit_id = req.deposit_transaction_id;
    let replay_or_conflict = |existing: SwapOrder| {
        let same_request = existing.deposit_transaction_id == deposit_id
            && existing.source_asset == source_asset
            && existing.target_asset == target_asset
            && existing.amount == amount;
        if same_request {
            Ok((existing, false))
        } else {
            Err(AppError::SwapConflict(
                "idempotency_key was already used for a different swap request".to_string(),
            ))
        }
    };

    if let Some(existing) =
        queries::get_swap_order_by_idempotency_key(pool, tenant_id, &idempotency_key).await?
    {
        return replay_or_conflict(existing);
    }

    let inserted = queries::insert_swap_order(
        pool,
        tenant_id,
        &idempotency_key,
        deposit_id,
        &source_asset,
        &target_asset,
        &amount,
    )
    .await;
    match inserted {
        Ok(Some(order)) => Ok((order, true)),
        // A concurrent request with the same key won the insert; replay its order.
        Ok(None) => {
            let existing =
                queries::get_swap_order_by_idempotency_key(pool, tenant_id, &idempotency_key)
                    .await?
                    .ok_or_else(|| {
                        AppError::Internal("idempotent swap order vanished during create".into())
                    })?;
            replay_or_conflict(existing)
        }
        // The deposit already backs another order (partial unique index).
        Err(e) if is_unique_violation(&e) => Err(AppError::SwapConflict(
            "deposit transaction already backs a swap order".to_string(),
        )),
        Err(e) => Err(e.into()),
    }
}

/// Fetch one order owned by `tenant_id`.
pub async fn get_order(pool: &PgPool, id: Uuid, tenant_id: Uuid) -> Result<SwapOrder, AppError> {
    queries::get_swap_order_for_tenant(pool, id, tenant_id)
        .await
        .map_err(|e| not_found_or(e, id))
}

/// One page of `tenant_id`'s orders (newest first) plus the total count.
pub async fn list_orders(
    pool: &PgPool,
    tenant_id: Uuid,
    page: &PaginationHelper,
) -> Result<(Vec<SwapOrder>, u64), AppError> {
    let total = queries::count_swap_orders_for_tenant(pool, tenant_id).await?;
    let orders = queries::list_swap_orders_for_tenant(
        pool,
        tenant_id,
        i64::from(page.limit()),
        i64::from(page.offset()),
    )
    .await?;
    Ok((orders, total as u64))
}

/// Cancel an order owned by `tenant_id`. Only `pending` orders can be cancelled;
/// anything else is a 409 that names the current status.
pub async fn cancel_order(pool: &PgPool, id: Uuid, tenant_id: Uuid) -> Result<SwapOrder, AppError> {
    if let Some(order) = queries::cancel_swap_order(pool, id, tenant_id).await? {
        return Ok(order);
    }
    // No pending row matched: either the order is missing or not cancellable.
    let current = get_order(pool, id, tenant_id).await?;
    Err(AppError::SwapNotCancellable(format!(
        "order is '{}', only '{STATUS_PENDING}' orders can be cancelled",
        current.status
    )))
}

fn validate_asset_code(field: &str, code: &str) -> Result<String, AppError> {
    let code = code.trim();
    let valid = (1..=12).contains(&code.len())
        && code
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
    if valid {
        Ok(code.to_string())
    } else {
        Err(AppError::Validation(format!(
            "{field} must be 1-12 uppercase letters or digits"
        )))
    }
}

fn parse_amount(raw: &str) -> Result<BigDecimal, AppError> {
    BigDecimal::from_str(raw.trim())
        .map_err(|_| AppError::Validation(format!("amount '{raw}' is not a decimal number")))
}

fn not_found_or(e: sqlx::Error, id: Uuid) -> AppError {
    match e {
        sqlx::Error::RowNotFound => AppError::NotFound(format!("Swap order {id} not found")),
        other => other.into(),
    }
}

fn is_unique_violation(err: &sqlx::Error) -> bool {
    matches!(err, sqlx::Error::Database(db_err) if db_err.code().as_deref() == Some("23505"))
}
