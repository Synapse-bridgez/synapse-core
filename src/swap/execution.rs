//! Swap orders: idempotent creation from a quote, status rules and cancellation.

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::types::BigDecimal;
use sqlx::PgPool;
use uuid::Uuid;

use super::quote;
use crate::db::queries;
use crate::error::AppError;

/// Order lifecycle. Clients can only read this; it is never accepted as input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapOrderStatus {
    Pending,
    Executing,
    Completed,
    Failed,
    Canceled,
}

impl SwapOrderStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Executing => "executing",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "executing" => Some(Self::Executing),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "canceled" => Some(Self::Canceled),
            _ => None,
        }
    }

    /// Only orders that have not started executing can be canceled.
    pub fn is_cancelable(&self) -> bool {
        matches!(self, Self::Pending)
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SwapOrder {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub quote_id: Uuid,
    pub source_asset: String,
    pub dest_asset: String,
    pub source_amount: BigDecimal,
    pub dest_amount: BigDecimal,
    pub status: String,
    pub idempotency_key: String,
    pub request_hash: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Validates a client-supplied idempotency key.
pub fn validate_idempotency_key(key: &str) -> Result<(), AppError> {
    let ok = !key.is_empty()
        && key.len() <= 128
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.'));
    if ok {
        Ok(())
    } else {
        Err(AppError::Validation(
            "Idempotency-Key must be 1-128 characters of [A-Za-z0-9-_:.]".to_string(),
        ))
    }
}

fn request_hash(quote_id: Uuid) -> String {
    hex::encode(Sha256::digest(format!("create_order:{quote_id}").as_bytes()))
}

/// Creates an order from a quote. Replaying the same key and payload returns
/// the originally created order; the same key with a different payload is a
/// conflict.
pub async fn create_order(
    pool: &PgPool,
    tenant_id: Uuid,
    idempotency_key: &str,
    quote_id: Uuid,
) -> Result<SwapOrder, AppError> {
    validate_idempotency_key(idempotency_key)?;
    let hash = request_hash(quote_id);

    if let Some(existing) =
        queries::get_swap_order_by_key_for_tenant(pool, tenant_id, idempotency_key).await?
    {
        return replay(existing, &hash);
    }

    let quote = queries::get_swap_quote_for_tenant(pool, quote_id, tenant_id)
        .await?
        .ok_or_else(|| AppError::NotFound("Quote not found".to_string()))?;
    if quote.is_expired(Utc::now()) {
        return Err(AppError::SwapQuoteExpired);
    }
    quote::check_risk(&quote.source_amount)?;

    let now = Utc::now();
    let order = SwapOrder {
        id: Uuid::new_v4(),
        tenant_id,
        quote_id,
        source_asset: quote.source_asset,
        dest_asset: quote.dest_asset,
        source_amount: quote.source_amount,
        dest_amount: quote.dest_amount,
        status: SwapOrderStatus::Pending.as_str().to_string(),
        idempotency_key: idempotency_key.to_string(),
        request_hash: hash.clone(),
        created_at: now,
        updated_at: now,
    };
    match queries::insert_swap_order(pool, &order).await? {
        Some(inserted) => Ok(inserted),
        // Lost a race: either a concurrent request with the same key, or the
        // quote was already consumed by another order.
        None => match queries::get_swap_order_by_key_for_tenant(pool, tenant_id, idempotency_key)
            .await?
        {
            Some(existing) => replay(existing, &hash),
            None => Err(AppError::SwapIllegalTransition(
                "quote has already been used".to_string(),
            )),
        },
    }
}

fn replay(existing: SwapOrder, hash: &str) -> Result<SwapOrder, AppError> {
    if existing.request_hash == hash {
        Ok(existing)
    } else {
        Err(AppError::SwapIdempotencyConflict(
            "key was already used with a different request".to_string(),
        ))
    }
}

/// Cancels a tenant's order if its status allows it.
pub async fn cancel_order(pool: &PgPool, tenant_id: Uuid, id: Uuid) -> Result<SwapOrder, AppError> {
    let order = queries::get_swap_order_for_tenant(pool, id, tenant_id)
        .await?
        .ok_or_else(|| AppError::NotFound("Order not found".to_string()))?;
    let current = SwapOrderStatus::parse(&order.status);
    if !current.map(|s| s.is_cancelable()).unwrap_or(false) {
        return Err(AppError::SwapIllegalTransition(format!(
            "order in status '{}' cannot be canceled",
            order.status
        )));
    }
    // Conditional update guards against a concurrent status change.
    queries::cancel_swap_order(pool, id, tenant_id)
        .await?
        .ok_or_else(|| {
            AppError::SwapIllegalTransition("order status changed concurrently".to_string())
        })
}
