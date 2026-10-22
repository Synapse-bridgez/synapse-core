// Database query helpers.
//
// This module centralizes SQL access for the application. Each function takes a
// connection/pool handle and returns typed results, keeping SQL out of the
// domain logic.

use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::AppError;
use crate::swap::execution::{ExecutionAlgorithmKind, SlicePlan, SliceStatus, TerminalBehavior};

/// A persisted parent swap order that is being worked by an execution algorithm.
pub struct ParentOrderRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub sell_asset: String,
    pub buy_asset: String,
    pub total_quantity: i64,
    pub filled_quantity: i64,
    pub algorithm: ExecutionAlgorithmKind,
    pub window_seconds: i64,
    pub slice_count: i32,
    pub min_slice_size: i64,
    pub max_slice_size: i64,
    pub randomization_bps: i32,
    pub urgency: i32,
    pub terminal_behavior: TerminalBehavior,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub status: String,
}

/// A persisted child-order slice belonging to a parent order.
pub struct SliceRow {
    pub id: Uuid,
    pub parent_order_id: Uuid,
    pub sequence: i32,
    pub size: i64,
    pub visible_size: Option<i64>,
    pub scheduled_at: chrono::DateTime<chrono::Utc>,
    pub submitted_at: Option<chrono::DateTime<chrono::Utc>>,
    pub filled_quantity: i64,
    pub status: SliceStatus,
    pub child_order_id: Option<Uuid>,
}

/// Insert a new parent order and return its generated id.
pub async fn insert_parent_order(
    pool: &PgPool,
    tenant_id: Uuid,
    sell_asset: &str,
    buy_asset: &str,
    total_quantity: i64,
    algorithm: ExecutionAlgorithmKind,
    window_seconds: i64,
    slice_count: i32,
    min_slice_size: i64,
    max_slice_size: i64,
    randomization_bps: i32,
    urgency: i32,
    terminal_behavior: TerminalBehavior,
) -> Result<Uuid, AppError> {
    let id = sqlx::query_scalar::<_, Uuid>(
        r#"
        INSERT INTO swap_parent_orders (
            tenant_id, sell_asset, buy_asset, total_quantity, filled_quantity,
            algorithm, window_seconds, slice_count, min_slice_size, max_slice_size,
            randomization_bps, urgency, terminal_behavior, started_at, status
        )
        VALUES ($1, $2, $3, $4, 0, $5, $6, $7, $8, $9, $10, $11, $12, NOW(), 'active')
        RETURNING id
        "#,
    )
    .bind(tenant_id)
    .bind(sell_asset)
    .bind(buy_asset)
    .bind(total_quantity)
    .bind(algorithm)
    .bind(window_seconds)
    .bind(slice_count)
    .bind(min_slice_size)
    .bind(max_slice_size)
    .bind(randomization_bps)
    .bind(urgency)
    .bind(terminal_behavior)
    .fetch_one(pool)
    .await?;

    Ok(id)
}

/// Load a parent order by id.
pub async fn get_parent_order(pool: &PgPool, id: Uuid) -> Result<Option<ParentOrderRow>, AppError> {
    let row = sqlx::query_as::<_, ParentOrderRow>(
        r#"
        SELECT id, tenant_id, sell_asset, buy_asset, total_quantity, filled_quantity,
               algorithm, window_seconds, slice_count, min_slice_size, max_slice_size,
               randomization_bps, urgency, terminal_behavior, started_at, status
        FROM swap_parent_orders
        WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;

    Ok(row)
}

/// Persist the full slice plan for a parent order in a single transaction.
///
/// Slices are inserted with a `pending` status so an in-progress parent order
/// can be resumed after a restart by re-reading its plan.
pub async fn insert_slice_plan(
    pool: &PgPool,
    parent_order_id: Uuid,
    plan: &[SlicePlan],
) -> Result<(), AppError> {
    let mut tx: Transaction<'_, Postgres> = pool.begin().await?;

    for (sequence, slice) in plan.iter().enumerate() {
        sqlx::query(
            r#"
            INSERT INTO swap_order_slices (
                parent_order_id, sequence, size, visible_size, scheduled_at,
                filled_quantity, status
            )
            VALUES ($1, $2, $3, $4, $5, 0, 'pending')
            "#,
        )
        .bind(parent_order_id)
        .bind(sequence as i32)
        .bind(slice.size)
        .bind(slice.visible_size)
        .bind(slice.scheduled_at)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(())
}

/// Load all slices for a parent order, ordered by sequence.
pub async fn get_slices_for_parent(
    pool: &PgPool,
    parent_order_id: Uuid,
) -> Result<Vec<SliceRow>, AppError> {
    let rows = sqlx::query_as::<_, SliceRow>(
        r#"
        SELECT id, parent_order_id, sequence, size, visible_size, scheduled_at,
               submitted_at, filled_quantity, status, child_order_id
        FROM swap_order_slices
        WHERE parent_order_id = $1
        ORDER BY sequence ASC
        "#,
    )
    .bind(parent_order_id)
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

/// Mark a slice as submitted, linking it to the child order created through the
/// normal order submission path. Idempotent: only transitions from `pending`.
pub async fn mark_slice_submitted(
    pool: &PgPool,
    slice_id: Uuid,
    child_order_id: Uuid,
) -> Result<bool, AppError> {
    let result = sqlx::query(
        r#"
        UPDATE swap_order_slices
        SET status = 'submitted', child_order_id = $2, submitted_at = NOW()
        WHERE id = $1 AND status = 'pending'
        "#,
    )
    .bind(slice_id)
    .bind(child_order_id)
    .execute(pool)
    .await?;

    Ok(result.rows_affected() == 1)
}

/// Record a fill against a slice and aggregate it into the parent order's
/// filled quantity. Runs in a transaction so parent and child stay consistent.
pub async fn record_slice_fill(
    pool: &PgPool,
    slice_id: Uuid,
    fill_quantity: i64,
) -> Result<(), AppError> {
    let mut tx: Transaction<'_, Postgres> = pool.begin().await?;

    let parent_order_id = sqlx::query_scalar::<_, Uuid>(
        r#"
        UPDATE swap_order_slices
        SET filled_quantity = filled_quantity + $2,
            status = CASE
                WHEN filled_quantity + $2 >= size THEN 'filled'
                ELSE 'partially_filled'
            END
        WHERE id = $1
        RETURNING parent_order_id
        "#,
    )
    .bind(slice_id)
    .bind(fill_quantity)
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query(
        r#"
        UPDATE swap_parent_orders
        SET filled_quantity = filled_quantity + $2,
            status = CASE
                WHEN filled_quantity + $2 >= total_quantity THEN 'completed'
                ELSE status
            END
        WHERE id = $1
        "#,
    )
    .bind(parent_order_id)
    .bind(fill_quantity)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

/// Mark a parent order as finished (completed, cancelled, or expired).
pub async fn finalize_parent_order(pool: &PgPool, id: Uuid, status: &str) -> Result<(), AppError> {
    sqlx::query(
        r#"
        UPDATE swap_parent_orders
        SET status = $2
        WHERE id = $1
        "#,
    )
    .bind(id)
    .bind(status)
    .execute(pool)
    .await?;

    Ok(())
}
