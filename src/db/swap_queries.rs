//! Persistence layer for the Phase 2 swap domain (`swap_quotes`, `swap_orders`,
//! `swap_fills`, `swap_events`).
//!
//! Every function takes a `PgExecutor` so callers can pass either a pool or an
//! open transaction. A status transition and its audit entry should be written
//! in the same transaction:
//!
//! ```text
//! let mut tx = pool.begin().await?;
//! let order = update_swap_order_status(&mut *tx, id, created_at, "pending", "routed").await?;
//! if order.is_some() {
//!     append_swap_event(&mut *tx, &event).await?;
//! }
//! tx.commit().await?;
//! ```
//!
//! Partition routing: rows land in the monthly partition for their `created_at`.
//! Partitions are created ahead of time by `ensure_swap_partitions` (run from
//! `PartitionManager`). Inserting outside any existing partition fails with
//! SQLSTATE 23514.

use crate::db::models::{SwapEvent, SwapFill, SwapOrder, SwapQuote};
use chrono::{DateTime, Utc};
use sqlx::{PgExecutor, Result};
use uuid::Uuid;

/// Persist a swap quote. Quotes are immutable after insert.
pub async fn insert_swap_quote<'e>(
    executor: impl PgExecutor<'e>,
    quote: &SwapQuote,
) -> Result<SwapQuote> {
    sqlx::query_as::<_, SwapQuote>(
        "INSERT INTO swap_quotes (id, tenant_id, stellar_account, source_asset, destination_asset, \
         source_amount, destination_amount, rate, fee_amount, expires_at, created_at, metadata) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) \
         RETURNING *",
    )
    .bind(quote.id)
    .bind(quote.tenant_id)
    .bind(&quote.stellar_account)
    .bind(&quote.source_asset)
    .bind(&quote.destination_asset)
    .bind(&quote.source_amount)
    .bind(&quote.destination_amount)
    .bind(&quote.rate)
    .bind(&quote.fee_amount)
    .bind(quote.expires_at)
    .bind(quote.created_at)
    .bind(&quote.metadata)
    .fetch_one(executor)
    .await
}

/// Persist a new swap order. The referenced quote must already exist
/// (`quote_id`, `quote_created_at`), or the foreign key rejects the insert.
pub async fn insert_swap_order<'e>(
    executor: impl PgExecutor<'e>,
    order: &SwapOrder,
) -> Result<SwapOrder> {
    sqlx::query_as::<_, SwapOrder>(
        "INSERT INTO swap_orders (id, tenant_id, stellar_account, quote_id, quote_created_at, \
         source_asset, destination_asset, source_amount, filled_amount, status, created_at, \
         updated_at, metadata) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) \
         RETURNING *",
    )
    .bind(order.id)
    .bind(order.tenant_id)
    .bind(&order.stellar_account)
    .bind(order.quote_id)
    .bind(order.quote_created_at)
    .bind(&order.source_asset)
    .bind(&order.destination_asset)
    .bind(&order.source_amount)
    .bind(&order.filled_amount)
    .bind(&order.status)
    .bind(order.created_at)
    .bind(order.updated_at)
    .bind(&order.metadata)
    .fetch_one(executor)
    .await
}

/// Move an order from `expected_status` to `new_status`.
///
/// The `WHERE status = $expected` guard makes this an optimistic-concurrency
/// check in the database. Returns `Ok(None)` when the order does not exist or
/// is no longer in `expected_status`, so the caller can detect a lost race and
/// reload instead of overwriting.
pub async fn update_swap_order_status<'e>(
    executor: impl PgExecutor<'e>,
    order_id: Uuid,
    order_created_at: DateTime<Utc>,
    expected_status: &str,
    new_status: &str,
) -> Result<Option<SwapOrder>> {
    sqlx::query_as::<_, SwapOrder>(
        "UPDATE swap_orders SET status = $4, updated_at = NOW() \
         WHERE id = $1 AND created_at = $2 AND status = $3 \
         RETURNING *",
    )
    .bind(order_id)
    .bind(order_created_at)
    .bind(expected_status)
    .bind(new_status)
    .fetch_optional(executor)
    .await
}

/// Record an execution fill against an order.
pub async fn insert_swap_fill<'e>(
    executor: impl PgExecutor<'e>,
    fill: &SwapFill,
) -> Result<SwapFill> {
    sqlx::query_as::<_, SwapFill>(
        "INSERT INTO swap_fills (id, order_id, order_created_at, source_amount, \
         destination_amount, rate, fee_amount, external_ref, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         RETURNING *",
    )
    .bind(fill.id)
    .bind(fill.order_id)
    .bind(fill.order_created_at)
    .bind(&fill.source_amount)
    .bind(&fill.destination_amount)
    .bind(&fill.rate)
    .bind(&fill.fee_amount)
    .bind(&fill.external_ref)
    .bind(fill.created_at)
    .fetch_one(executor)
    .await
}

/// Append an entry to the order lifecycle log. The table rejects UPDATE and
/// DELETE, so this is the only write path after insert.
pub async fn append_swap_event<'e>(
    executor: impl PgExecutor<'e>,
    event: &SwapEvent,
) -> Result<SwapEvent> {
    sqlx::query_as::<_, SwapEvent>(
        "INSERT INTO swap_events (id, order_id, order_created_at, from_status, to_status, \
         payload, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         RETURNING *",
    )
    .bind(event.id)
    .bind(event.order_id)
    .bind(event.order_created_at)
    .bind(&event.from_status)
    .bind(&event.to_status)
    .bind(&event.payload)
    .bind(event.created_at)
    .fetch_one(executor)
    .await
}
