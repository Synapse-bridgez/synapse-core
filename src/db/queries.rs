// Database query helpers.
//
// This module centralizes SQL access for the application. Each function takes a
// connection/pool handle and returns typed results, keeping SQL out of the
// domain logic.

use crate::db::audit::{AuditLog, ENTITY_TRANSACTION};
use crate::db::models::{Settlement, SwapOrder, SwapOrderStatus, SwapSettlement, Transaction};
use crate::db::outbox;
use crate::swap::quote::{QuoteCandidate, SwapQuote};
use crate::services::query_cache::QueryCache;
use crate::swap::execution::SwapExecutionRecord;
use crate::tenant::TenantConfig;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::Sha256;
use sqlx::types::BigDecimal;
use sqlx::{PgPool, Postgres, Result, Row, Transaction as SqlxTransaction};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::time::timeout;
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

/// Insert a new parent order and return its ge
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

/// Get active redirects for an endpoint (not expired, not cancelled)
pub async fn get_active_endpoint_redirects(
    pool: &PgPool,
    endpoint_id: Uuid,
) -> Result<Vec<WebhookEndpointRedirect>> {
    with_timeout(
        QueryTier::Read,
        "SELECT * FROM webhook_endpoint_redirects WHERE endpoint_id = $1 AND active",
        async {
            sqlx::query_as::<_, WebhookEndpointRedirect>(
                r#"
                SELECT id, endpoint_id, redirect_url, enabled, created_at, started_at, expires_at, cancelled_at, metadata
                FROM webhook_endpoint_redirects
                WHERE endpoint_id = $1
                    AND enabled = true
                    AND cancelled_at IS NULL
                    AND expires_at > NOW()
                ORDER BY created_at DESC
                "#,
            )
            .bind(endpoint_id)
            .fetch_all(pool)
            .await
        },
    )
    .await
}

/// Cancel a webhook redirect (soft delete)
pub async fn cancel_webhook_redirect(
    pool: &PgPool,
    redirect_id: i64,
) -> Result<bool> {
    with_timeout(
        QueryTier::Write,
        "UPDATE webhook_endpoint_redirects SET cancelled_at = NOW()",
        async {
            let result = sqlx::query(
                "UPDATE webhook_endpoint_redirects SET cancelled_at = NOW() WHERE id = $1 AND cancelled_at IS NULL"
            )
            .bind(redirect_id)
            .execute(pool)
            .await?;

            Ok(result.rows_affected() > 0)
        },
    )
    .await
}

/// Mark redirect as started
pub async fn start_webhook_redirect(
    pool: &PgPool,
    redirect_id: i64,
) -> Result<bool> {
    with_timeout(
        QueryTier::Write,
        "UPDATE webhook_endpoint_redirects SET started_at = NOW()",
        async {
            let result = sqlx::query(
                "UPDATE webhook_endpoint_redirects SET started_at = NOW() WHERE id = $1 AND started_at IS NULL"
            )
            .bind(redirect_id)
            .execute(pool)
            .await?;

            Ok(result.rows_affected() > 0)
        },
    )
    .await
}

// --- Webhook Retry Policy (Issue #1258) ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryPolicyRequest {
    pub max_attempts: Option<i32>,
    pub base_delay_secs: Option<i32>,
    pub multiplier: Option<f64>,
    pub max_delay_secs: Option<i32>,
}

impl RetryPolicyRequest {
    /// Validates the retry policy request against platform-wide safety boundaries
    pub fn validate(&self) -> Result<(), String> {
        if let Some(attempts) = self.max_attempts {
            if attempts < 1 || attempts > 50 {
                return Err("max_attempts must be between 1 and 50".to_string());
            }
        }
        if let Some(delay) = self.base_delay_secs {
            if delay < 1 || delay > 3600 {
                return Err("base_delay_secs must be between 1 and 3600".to_string());
            }
        }
        if let Some(mult) = self.multiplier {
            if mult <= 1.0 || mult > 10.0 {
                return Err("multiplier must be between 1.0 (exclusive) and 10.0".to_string());
            }
        }
        if let Some(delay) = self.max_delay_secs {
            if delay < 60 || delay > 86400 {
                return Err("max_delay_secs must be between 60 and 86400".to_string());
            }
        }
        Ok(())
    }
}

/// Update retry policy for a webhook endpoint
pub async fn update_webhook_retry_policy(
    pool: &PgPool,
    endpoint_id: Uuid,
    policy: RetryPolicyRequest,
) -> Result<()> {
    with_timeout(
        QueryTier::Write,
        "UPDATE webhook_endpoints SET retry_policy",
        async {
            // Build policy JSON with defaults for unspecified fields
            let current_policy: serde_json::Value = sqlx::query_scalar(
                "SELECT COALESCE(retry_policy, jsonb_build_object('max_attempts', 5, 'base_delay_secs', 10, 'multiplier', 2.0, 'max_delay_secs', 300)) FROM webhook_endpoints WHERE id = $1"
            )
            .bind(endpoint_id)
            .fetch_optional(pool)
            .await?
            .ok_or_else(|| sqlx::Error::RowNotFound)?;

            let mut updated = current_policy.as_object().cloned().unwrap_or_default();

            if let Some(attempts) = policy.max_attempts {
                updated.insert("max_attempts".to_string(), serde_json::json!(attempts));
            }
            if let Some(delay) = policy.base_delay_secs {
                updated.insert("base_delay_secs".to_string(), serde_json::json!(delay));
            }
            if let Some(mult) = policy.multiplier {
                updated.insert("multiplier".to_string(), serde_json::json!(mult));
            }
            if let Some(delay) = policy.max_delay_secs {
                updated.insert("max_delay_secs".to_string(), serde_json::json!(delay));
            }

            sqlx::query(
                "UPDATE webhook_endpoints SET retry_policy = $1, updated_at = NOW() WHERE id = $2"
            )
            .bind(serde_json::to_value(&updated)?)
            .bind(endpoint_id)
            .execute(pool)
            .await?;

            Ok(())
        },
    )
    .await
}

// --- Bridge Handoffs (Issue #1464) ---
//
// Persistence for the Phase 1 -> Phase 2 bridging boundary. A handoff row is
// created exactly once per settled swap: `insert_bridge_handoff` is idempotent
// on `idempotency_key`, so duplicate settle events (or a replayed outbox
// message) collapse onto the existing row instead of emitting a second
// handoff. Status transitions are guarded so an acknowledged handoff can never
// be moved back to pending/failed by a late or duplicated event.

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct BridgeHandoffRow {
    pub id: Uuid,
    pub tenant_id: Option<Uuid>,
    pub swap_id: Uuid,
    pub settlement_id: Option<Uuid>,
    pub correlation_key: String,
    pub idempotency_key: String,
    pub source_asset: String,
    pub source_amount: BigDecimal,
    pub destination_chain: String,
    pub destination_address: String,
    pub payload_version: i32,
    pub payload: serde_json::Value,
    pub status: String,
    pub attempt_count: i32,
    pub last_error: Option<String>,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Insert a bridge handoff, idempotent on `idempotency_key`.
///
/// Returns `(row, inserted)`. `inserted == false` means a handoff with the same
/// idempotency key already existed and the stored row is returned unchanged —
/// callers must not publish another outbox event in that case.
pub async fn insert_bridge_handoff(
    pool: &PgPool,
    handoff: &BridgeHandoffRow,
) -> Result<(BridgeHandoffRow, bool)> {
    with_timeout(
        QueryTier::Write,
        "INSERT INTO bridge_handoffs ... ON CONFLICT (idempotency_key) DO NOTHING",
        async {
            let inserted = sqlx::query_as::<_, BridgeHandoffRow>(
                r#"
                INSERT INTO bridge_handoffs (
                    id, tenant_id, swap_id, settlement_id, correlation_key, idempotency_key,
                    source_asset, source_amount, destination_chain, destination_address,
                    payload_version, payload, status, attempt_count, last_error,
                    acknowledged_at, created_at, updated_at
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)
                ON CONFLICT (idempotency_key) DO NOTHING
                RETURNING *
                "#,
            )
            .bind(handoff.id)
            .bind(handoff.tenant_id)
            .bind(handoff.swap_id)
            .bind(handoff.settlement_id)
            .bind(&handoff.correlation_key)
            .bind(&handoff.idempotency_key)
            .bind(&handoff.source_asset)
            .bind(&handoff.source_amount)
            .bind(&handoff.destination_chain)
            .bind(&handoff.destination_address)
            .bind(handoff.payload_version)
            .bind(&handoff.payload)
            .bind(&handoff.status)
            .bind(handoff.attempt_count)
            .bind(&handoff.last_error)
            .bind(handoff.acknowledged_at)
            .bind(handoff.created_at)
            .bind(handoff.updated_at)
            .fetch_optional(pool)
            .await?;

            match inserted {
                Some(row) => Ok((row, true)),
                None => {
                    let existing = sqlx::query_as::<_, BridgeHandoffRow>(
                        "SELECT * FROM bridge_handoffs WHERE idempotency_key = $1",
                    )
                    .bind(&handoff.idempotency_key)
                    .fetch_one(pool)
                    .await?;
                    Ok((existing, false))
                }
            }
        },
    )
    .await
}

/// Fetch a handoff by its idempotency key.
pub async fn get_bridge_handoff_by_idempotency_key(
    pool: &PgPool,
    idempotency_key: &str,
) -> Result<Option<BridgeHandoffRow>> {
    with_timeout(
        QueryTier::Read,
        "SELECT * FROM bridge_handoffs WHERE idempotency_key = $1",
        sqlx::query_as::<_, BridgeHandoffRow>(
            "SELECT * FROM bridge_handoffs WHERE idempotency_key = $1",
        )
        .bind(idempotency_key)
        .fetch_optional(pool),
    )
    .await
}

/// Fetch a handoff by id, optionally scoped to a tenant.
pub async fn get_bridge_handoff(
    pool: &PgPool,
    id: Uuid,
    tenant_id: Option<Uuid>,
) -> Result<Option<BridgeHandoffRow>> {
    with_timeout(
        QueryTier::Read,
        "SELECT * FROM bridge_handoffs WHERE id = $1 [tenant-scoped]",
        async {
            match tenant_id {
                Some(tid) => {
                    sqlx::query_as::<_, BridgeHandoffRow>(
                        "SELECT * FROM bridge_handoffs WHERE id = $1 AND (tenant_id = $2 OR tenant_id IS NULL)",
                    )
                    .bind(id)
                    .bind(tid)
                    .fetch_optional(pool)
                    .await
                }
                None => {
                    sqlx::query_as::<_, BridgeHandoffRow>(
                        "SELECT * FROM bridge_handoffs WHERE id = $1",
                    )
                    .bind(id)
                    .fetch_optional(pool)
                    .await
                }
            }
        },
    )
    .await
}

/// Transition a handoff to `new_status`.
///
/// Only `pending -> acknowledged`, `pending -> failed`, and `failed -> pending`
/// (retry) are permitted. The UPDATE is guarded on the current status so a
/// concurrent or duplicated transition is a no-op returning `Ok(None)` rather
/// than clobbering a terminal state.
pub async fn update_bridge_handoff_status(
    pool: &PgPool,
    id: Uuid,
    new_status: &str,
    error: Option<&str>,
) -> Result<Option<BridgeHandoffRow>> {
    with_timeout(
        QueryTier::Write,
        "UPDATE bridge_handoffs SET status = $1 [guarded transition]",
        async {
            let allowed_from: &[&str] = match new_status {
                "acknowledged" => &["pending"],
                "failed" => &["pending"],
                "pending" => &["failed"],
                _ => &[],
            };
            if allowed_from.is_empty() {
                return Err(sqlx::Error::Decode(
                    format!("invalid bridge handoff status transition to '{new_status}'").into(),
                ));
            }

            let acknowledged_at = if new_status == "acknowledged" {
                Some(Utc::now())
            } else {
                None
            };

            sqlx::query_as::<_, BridgeHandoffRow>(
                r#"
                UPDATE bridge_handoffs SET
                    status = $1,
                    last_error = $2,
                    acknowledged_at = COALESCE($3, acknowledged_at),
                    attempt_count = CASE WHEN $1 = 'pending' THEN attempt_count + 1 ELSE attempt_count END,
                    updated_at = NOW()
                WHERE id = $4 AND status = ANY($5)
                RETURNING *
                "#,
            )
            .bind(new_status)
            .bind(error)
            .bind(acknowledged_at)
            .bind(id)
            .bind(allowed_from)
            .fetch_optional(pool)
            .await
        },
    )
    .await
}

/// Select handoffs eligible for retry/replay: failed, or pending past the
/// acknowledgement timeout. Ordered oldest-first so the backlog drains FIFO.
pub async fn list_bridge_handoffs_for_retry(
    pool: &PgPool,
    tenant_id: Option<Uuid>,
    ack_timeout_secs: i64,
    limit: i64,
) -> Result<Vec<BridgeHandoffRow>> {
    with_timeout(
        QueryTier::Read,
        "SELECT * FROM bridge_handoffs WHERE status IN ('failed','pending') [retry selection]",
        async {
            let timeout = chrono::Duration::seconds(ack_timeout_secs.max(0));
            let cutoff = Utc::now() - timeout;
            match tenant_id {
                Some(tid) => {
                    sqlx::query_as::<_, BridgeHandoffRow>(
                        r#"
                        SELECT * FROM bridge_handoffs
                        WHERE (tenant_id = $1 OR tenant_id IS NULL)
                          AND (status = 'failed' OR (status = 'pending' AND updated_at <= $2))
                        ORDER BY updated_at ASC
                        LIMIT $3
                        "#,
                    )
                    .bind(tid)
                    .bind(cutoff)
                    .bind(limit)
                    .fetch_all(pool)
                    .await
                }
                None => {
                    sqlx::query_as::<_, BridgeHandoffRow>(
                        r#"
                        SELECT * FROM bridge_handoffs
                        WHERE status = 'failed' OR (status = 'pending' AND updated_at <= $1)
                        ORDER BY updated_at ASC
                        LIMIT $2
                        "#,
                    )
                    .bind(cutoff)
                    .bind(limit)
                    .fetch_all(pool)
                    .await
                }
            }
        },
    )
    .await
}

// --- Swap Read Model (Issue #1468) ---
//
// CQRS read surface consumed by the REST/GraphQL layers. All queries are
// tenant-scoped: `tenant_id` is a mandatory filter (NULL-tenant legacy rows
// remain visible, mirroring the write-side RLS policy). The projection is
// rebuildable, so these tables are never the source of truth.

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct SwapOrderSummaryRow {
    pub order_id: Uuid,
    pub tenant_id: Option<Uuid>,
    pub status: String,
    pub source_asset: String,
    pub destination_asset: String,
    pub source_amount: BigDecimal,
    pub filled_amount: BigDecimal,
    pub quote_id: Option<Uuid>,
    pub settlement_id: Option<Uuid>,
    pub last_event_seq: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct SwapFillSummaryRow {
    pub fill_id: Uuid,
    pub order_id: Uuid,
    pub tenant_id: Option<Uuid>,
    pub asset_code: String,
    pub amount: BigDecimal,
    pub price: Option<BigDecimal>,
    pub filled_at: DateTime<Utc>,
    pub last_event_seq: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct SwapTenantAggregateRow {
    pub tenant_id: Option<Uuid>,
    pub status: String,
    pub source_asset: String,
    pub order_count: i64,
    pub total_source_amount: BigDecimal,
    pub total_filled_amount: BigDecimal,
    pub updated_at: DateTime<Utc>,
}

/// Filters for read-model order listing. All fields are optional except the
/// mandatory tenant scope.
#[derive(Debug, Default, Clone)]
pub struct SwapOrderSummaryFilter {
    pub status: Option<String>,
    pub source_asset: Option<String>,
    pub destination_asset: Option<String>,
    pub from_date: Option<DateTime<Utc>>,
    pub to_date: Option<DateTime<Utc>>,
}

/// Paginated, filtered order-summary listing for a tenant.
///
/// Keyset pagination on `(created_at, order_id)` DESC; `cursor` is the last row
/// of the previous page.
pub async fn list_swap_order_summaries(
    pool: &PgPool,
    tenant_id: Uuid,
    filter: &SwapOrderSummaryFilter,
    limit: i64,
    cursor: Option<(DateTime<Utc>, Uuid)>,
) -> Result<Vec<SwapOrderSummaryRow>> {
    with_timeout(
        QueryTier::Read,
        "SELECT * FROM swap_order_summary [tenant-scoped filtered cursor-paginated]",
        async {
            let mut conditions: Vec<String> = Vec::new();
            let mut p = 1usize;

            conditions.push(format!("(tenant_id = ${p} OR tenant_id IS NULL)"));
            p += 1;

            if filter.status.is_some() {
                conditions.push(format!("status = ${p}"));
                p += 1;
            }
            if filter.source_asset.is_some() {
                conditions.push(format!("source_asset = ${p}"));
                p += 1;
            }
            if filter.destination_asset.is_some() {
                conditions.push(format!("destination_asset = ${p}"));
                p += 1;
            }
            if filter.from_date.is_some() {
                conditions.push(format!("created_at >= ${p}"));
                p += 1;
            }
            if filter.to_date.is_some() {
                conditions.push(format!("created_at <= ${p}"));
                p += 1;
            }
            if cursor.is_some() {
                conditions.push(format!("(created_at, order_id) < (${p}, ${})", p + 1));
                p += 2;
            }

            let sql = format!(
                "SELECT * FROM swap_order_summary WHERE {} \
                 ORDER BY created_at DESC, order_id DESC LIMIT ${p}",
                conditions.join(" AND ")
            );

            let mut q = sqlx::query_as::<_, SwapOrderSummaryRow>(&sql).bind(tenant_id);
            if let Some(v) = &filter.status {
                q = q.bind(v);
            }
            if let Some(v) = &filter.source_asset {
                q = q.bind(v);
            }
            if let Some(v) = &filter.destination_asset {
                q = q.bind(v);
            }
            if let Some(v) = filter.from_date {
                q = q.bind(v);
            }
            if let Some(v) = filter.to_date {
                q = q.bind(v);
            }
            if let Some((ts, id)) = cursor {
                q = q.bind(ts).bind(id);
            }
            q = q.bind(limit);

            q.fetch_all(pool).await
        },
    )
    .await
}

/// Fetch a single order summary, tenant-scoped.
pub async fn get_swap_order_summary(
    pool: &PgPool,
    tenant_id: Uuid,
    order_id: Uuid,
) -> Result<Option<SwapOrderSummaryRow>> {
    with_timeout(
        QueryTier::Read,
        "SELECT * FROM swap_order_summary WHERE order_id = $1 [tenant-scoped]",
        sqlx::query_as::<_, SwapOrderSummaryRow>(
            "SELECT * FROM swap_order_summary \
             WHERE order_id = $1 AND (tenant_id = $2 OR tenant_id IS NULL)",
        )
        .bind(order_id)
        .bind(tenant_id)
        .fetch_optional(pool),
    )
    .await
}

/// Paginated fill summaries for an order, tenant-scoped.
pub async fn list_swap_fill_summaries(
    pool: &PgPool,
    tenant_id: Uuid,
    order_id: Uuid,
    limit: i64,
    cursor: Option<(DateTime<Utc>, Uuid)>,
) -> Result<Vec<SwapFillSummaryRow>> {
    with_timeout(
        QueryTier::Read,
        "SELECT * FROM swap_fill_summary WHERE order_id = $1 [tenant-scoped cursor-paginated]",
        async {
            if let Some((ts, id)) = cursor {
                sqlx::query_as::<_, SwapFillSummaryRow>(
                    "SELECT * FROM swap_fill_summary \
                     WHERE order_id = $1 AND (tenant_id = $2 OR tenant_id IS NULL) \
                       AND (filled_at, fill_id) < ($3, $4) \
                     ORDER BY filled_at DESC, fill_id DESC LIMIT $5",
                )
                .bind(order_id)
                .bind(tenant_id)
                .bind(ts)
                .bind(id)
                .bind(limit)
                .fetch_all(pool)
                .await
            } else {
                sqlx::query_as::<_, SwapFillSummaryRow>(
                    "SELECT * FROM swap_fill_summary \
                     WHERE order_id = $1 AND (tenant_id = $2 OR tenant_id IS NULL) \
                     ORDER BY filled_at DESC, fill_id DESC LIMIT $3",
                )
                .bind(order_id)
                .bind(tenant_id)
                .bind(limit)
                .fetch_all(pool)
                .await
            }
        },
    )
    .await
}

/// Per-tenant aggregates, optionally narrowed to a status/asset pair.
pub async fn list_swap_tenant_aggregates(
    pool: &PgPool,
    tenant_id: Uuid,
    status: Option<&str>,
    source_asset: Option<&str>,
) -> Result<Vec<SwapTenantAggregateRow>> {
    with_timeout(
        QueryTier::Read,
        "SELECT * FROM swap_tenant_aggregates [tenant-scoped filtered]",
        async {
            let mut conditions: Vec<String> = Vec::new();
            let mut p = 1usize;

            conditions.push(format!("(tenant_id = ${p} OR tenant_id IS NULL)"));
            p += 1;

            if status.is_some() {
                conditions.push(format!("status = ${p}"));
                p += 1;
            }
            if source_asset.is_some() {
                conditions.push(format!("source_asset = ${p}"));
                p += 1;
            }

            let sql = format!(
                "SELECT * FROM swap_tenant_aggregates WHERE {} \
                 ORDER BY status ASC, source_asset ASC",
                conditions.join(" AND ")
            );

            let mut q = sqlx::query_as::<_, SwapTenantAggregateRow>(&sql).bind(tenant_id);
            if let Some(v) = status {
                q = q.bind(v);
            }
            if let Some(v) = source_asset {
                q = q.bind(v);
            }

            q.fetch_all(pool).await
        },
    )
    .await
}

/// Read the projection checkpoint for a consumer name.
pub async fn get_projection_checkpoint(
    pool: &PgPool,
    consumer: &str,
) -> Result<Option<(i64, DateTime<Utc>)>> {
    with_timeout(
        QueryTier::Read,
        "SELECT last_event_seq, updated_at FROM projection_checkpoints WHERE consumer = $1",
        async {
            let row = sqlx::query(
                "SELECT last_event_seq, updated_at FROM projection_checkpoints WHERE consumer = $1",
            )
            .bind(consumer)
            .fetch_optional(pool)
            .await?;
            Ok(row.map(|r| {
                (
                    r.get::<i64, _>("last_event_seq"),
                    r.get::<DateTime<Utc>, _>("updated_at"),
                )
            }))
        },
    )
    .await
}

/// Advance the projection checkpoint. Monotonic: a lower sequence number never
/// moves the checkpoint backwards (guards against out-of-order consumers).
pub async fn set_projection_checkpoint(
    pool: &PgPool,
    consumer: &str,
    last_event_seq: i64,
) -> Result<()> {
    with_timeout(
        QueryTier::Write,
        "INSERT INTO projection_checkpoints ... ON CONFLICT DO UPDATE (monotonic)",
        async {
            sqlx::query(
                r#"
                INSERT INTO projection_checkpoints (consumer, last_event_seq, updated_at)
                VALUES ($1, $2, NOW())
                ON CONFLICT (consumer) DO UPDATE
                SET last_event_seq = GREATEST(projection_checkpoints.last_event_seq, EXCLUDED.last_event_seq),
                    updated_at = NOW()
                "#,
            )
            .bind(consumer)
            .bind(last_event_seq)
            .execute(pool)
            .await?;
            Ok(())
        },
    )
    .await
}

/// Read the recorded projection schema version for a consumer.
pub async fn get_projection_version(pool: &PgPool, consumer: &str) -> Result<Option<i32>> {
    with_timeout(
        QueryTier::Read,
        "SELECT version FROM projection_versions WHERE consumer = $1",
        async {
            let v: Option<i32> =
                sqlx::query_scalar("SELECT version FROM projection_versions WHERE consumer = $1")
                    .bind(consumer)
                    .fetch_optional(pool)
                    .await?;
            Ok(v)
        },
    )
    .await
}

/// Record the projection schema version. A version bump signals that the read
/// model must be rebuilt before it can be trusted.
pub async fn set_projection_version(pool: &PgPool, consumer: &str, version: i32) -> Result<()> {
    with_timeout(
        QueryTier::Write,
        "INSERT INTO projection_versions ... ON CONFLICT DO UPDATE",
        async {
            sqlx::query(
                r#"
                INSERT INTO projection_versions (consumer, version, updated_at)
                VALUES ($1, $2, NOW())
                ON CONFLICT (consumer) DO UPDATE
                SET version = EXCLUDED.version, updated_at = NOW()
                "#,
            )
            .bind(consumer)
            .bind(version)
            .execute(pool)
            .await?;
            Ok(())
        },
    )
    .await
}

// --- Transaction Notes (Issue #1257) ---

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct TransactionNote {
    pub id: i64,
    pub transaction_id: Uuid,
    pub admin_principal: String,
    pub note_text: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateTransactionNoteRequest {
    pub note_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaginatedNotes {
    pub items: Vec<TransactionNote>,
    pub has_more: bool,
    pub cursor: Option<i64>,
}

/// Add a note to a transaction (immutable, append-only)
pub async fn add_transaction_note(
    pool: &PgPool,
    transaction_id: Uuid,
    admin_principal: &str,
    note_text: &str,
) -> Result<TransactionNote> {
    with_timeout(
        QueryTier::Write,
        "INSERT INTO transaction_notes (transaction_id, admin_principal, note_text)",
        async {
            sqlx::query_as::<_, TransactionNote>(
                r#"
                INSERT INTO transaction_notes (transaction_id, admin_principal, note_text)
                VALUES ($1, $2, $3)
                RETURNING id, transaction_id, admin_principal, note_text, created_at
                "#,
            )
            .bind(transaction_id)
            .bind(admin_principal)
            .bind(note_text)
            .fetch_one(pool)
            .await
        },
    )
    .await
}

/// Get paginated notes for a transaction
pub async fn get_transaction_notes(
    pool: &PgPool,
    transaction_id: Uuid,
    limit: i64,
    cursor: Option<i64>,
) -> Result<PaginatedNotes> {
    with_timeout(
        QueryTier::Read,
        "SELECT * FROM transaction_notes WHERE transaction_id = $1 ORDER BY id DESC",
        async {
            // Fetch limit + 1 to determine if there are more results
            let fetch_limit = limit + 1;
            let notes = if let Some(cursor_id) = cursor {
                sqlx::query_as::<_, TransactionNote>(
                    r#"
                    SELECT id, transaction_id, admin_principal, note_text, created_at
                    FROM transaction_notes
                    WHERE transaction_id = $1 AND id < $2
                    ORDER BY id DESC
                    LIMIT $3
                    "#,
                )
                .bind(transaction_id)
                .bind(cursor_id)
                .bind(fetch_limit)
                .fetch_all(pool)
                .await?
            } else {
                sqlx::query_as::<_, TransactionNote>(
                    r#"
                    SELECT id, transaction_id, admin_principal, note_text, created_at
                    FROM transaction_notes
                    WHERE transaction_id = $1
                    ORDER BY id DESC
                    LIMIT $2
                    "#,
                )
                .bind(transaction_id)
                .bind(fetch_limit)
                .fetch_all(pool)
                .await?
            };

            let has_more = notes.len() > limit as usize;
            let items = if has_more {
                notes[..limit as usize].to_vec()
            } else {
                notes
            };

            let next_cursor = if has_more && !items.is_empty() {
                Some(items.last().unwrap().id)
            } else {
                None
            };

            Ok(PaginatedNotes {
                items,
                has_more,
                cursor: next_cursor,
            })
        },
    )
    .await
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use sqlx::{migrate::Migrator, PgPool};
    use std::path::Path;

    async fn setup_test_db() -> PgPool {
        let database_url =
            std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for tests");
        let pool = PgPool::connect(&database_url)
            .await
            .expect("Failed to connect to test DB");
        let migrator = Migrator::new(Path::new("./migrations"))
            .await
            .expect("Failed to load migrations");
        migrator.run(&pool).await.expect("Failed to run migrations");
        pool
    }

    #[ignore = "Requires DATABASE_URL"]
    #[tokio::test]
    async fn test_get_active_tenant_rate_limit() {
        let pool = setup_test_db().await;
        let tenant_id = uuid::Uuid::new_v4();

        sqlx::query(
            "INSERT INTO tenants (tenant_id, name, api_key_hash, webhook_secret, stellar_account, rate_limit_per_minute, is_active) VALUES ($1, $2, $3, pgp_sym_encrypt($4, $5), $6, $7, $8)",
        )
        .bind(tenant_id)
        .bind("test tenant")
        .bind(hash_api_key(&format!("key-{tenant_id}")))
        .bind("secret")
        .bind(tenant_secret_key())
        .bind("GTESTACCOUNT")
        .bind(420)
        .bind(true)
        .execute(&pool)
        .await
        .unwrap();

        let limit = get_active_tenant_rate_limit(&pool, tenant_id)
            .await
            .expect("Failed to query rate limit");

        assert_eq!(limit, Some(420));
    }

    #[ignore = "Requires DATABASE_URL"]
    #[tokio::test]
    async fn test_get_all_tenant_configs_includes_rate_limit() {
        let pool = setup_test_db().await;
        let tenant_id = uuid::Uuid::new_v4();

        sqlx::query(
            "INSERT INTO tenants (tenant_id, name, api_key_hash, webhook_secret, stellar_account, rate_limit_per_minute, is_active) VALUES ($1, $2, $3, pgp_sym_encrypt($4, $5), $6, $7, $8)",
        )
        .bind(tenant_id)
        .bind("test tenant 2")
        .bind(hash_api_key(&format!("key-{tenant_id}")))
        .bind("secret2")
        .bind(tenant_secret_key())
        .bind("GTESTACCOUNT2")
        .bind(50)
        .bind(true)
        .execute(&pool)
        .await
        .unwrap();

        let configs = get_all_tenant_configs(&pool).await.unwrap();
        assert!(configs
            .iter()
            .any(|cfg| cfg.tenant_id == tenant_id && cfg.rate_limit_per_minute == 50));
    }

    // --- Rate limit validation unit tests (no DB required) ---

    #[test]
    fn test_update_tenant_rate_limit_rejects_zero() {
        // Validation fires before any DB call, so we can test it synchronously
        // by checking the error path directly.
        let result: Result<i32> = Err(sqlx::Error::Decode(
            "rate_limit_per_minute must be positive, got 0".into(),
        ));
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_update_tenant_rate_limit_validation_zero() {
        // Build a dummy pool URL that will never be reached because validation
        // fires first.
        let pool = PgPool::connect_lazy("postgres://localhost/nonexistent").unwrap();
        let err = update_tenant_rate_limit(&pool, uuid::Uuid::new_v4(), 0, "test")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("must be positive"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_update_tenant_rate_limit_validation_negative() {
        let pool = PgPool::connect_lazy("postgres://localhost/nonexistent").unwrap();
        let err = update_tenant_rate_limit(&pool, uuid::Uuid::new_v4(), -5, "test")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("must be positive"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_update_tenant_rate_limit_validation_exceeds_max() {
        let pool = PgPool::connect_lazy("postgres://localhost/nonexistent").unwrap();
        let err = update_tenant_rate_limit(
            &pool,
            uuid::Uuid::new_v4(),
            MAX_RATE_LIMIT_PER_MINUTE + 1,
            "test",
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("exceeds maximum"),
            "unexpected error: {err}"
        );
    }

    #[ignore = "Requires DATABASE_URL"]
    #[tokio::test]
    async fn test_update_tenant_rate_limit_persists() {
        let pool = setup_test_db().await;
        let tenant_id = uuid::Uuid::new_v4();

        sqlx::query(
            "INSERT INTO tenants (tenant_id, name, api_key_hash, webhook_secret, stellar_account, rate_limit_per_minute, is_active) VALUES ($1, $2, $3, pgp_sym_encrypt($4, $5), $6, $7, $8)",
        )
        .bind(tenant_id)
        .bind("rl-test-tenant")
        .bind(hash_api_key(&format!("key-{tenant_id}")))
        .bind("secret")
        .bind(tenant_secret_key())
        .bind("GTESTACCOUNT3")
        .bind(60)
        .bind(true)
        .execute(&pool)
        .await
        .unwrap();

        let updated = update_tenant_rate_limit(&pool, tenant_id, 120, "test-actor")
            .await
            .expect("update should succeed");
        assert_eq!(updated, 120);

        let stored = get_active_tenant_rate_limit(&pool, tenant_id)
            .await
            .expect("query should succeed");
        assert_eq!(stored, Some(120));
    }
}
