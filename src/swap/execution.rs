//! Swap order execution engine.
//!
//! A worker loop leases executable orders (`FOR UPDATE SKIP LOCKED` plus a
//! visibility-timeout lease), submits them to the order's [`ExecutionVenue`]
//! with an idempotency key derived from the order ID, records fills in
//! `swap_fills`, and moves status only through the domain transition guard
//! with a conditional (compare-and-set) update.
//!
//! Timeouts where the venue outcome is unknown move the order to
//! `reconciling`; from there the engine only *looks up* the venue state by
//! idempotency key and never re-submits blindly.

use async_trait::async_trait;
use serde_json::json;
use sqlx::types::BigDecimal;
use sqlx::{PgPool, Row};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

use super::events::{record_event, SwapEventType};
use super::order::*;

/// A single fill reported by a venue. `venue_fill_id` must be stable across
/// repeated reports of the same fill.
#[derive(Debug, Clone)]
pub struct VenueFill {
    pub venue_fill_id: String,
    pub amount: BigDecimal,
    pub price: BigDecimal,
}

/// Typed result of a venue interaction.
#[derive(Debug, Clone)]
pub enum VenueOutcome {
    /// Order completely filled (fills may be cumulative).
    Filled(Vec<VenueFill>),
    /// Order partially filled; more execution is needed.
    PartiallyFilled(Vec<VenueFill>),
    /// Retryable failure; the order definitely was not executed.
    Transient(String),
    /// Non-retryable failure with a venue reason.
    Permanent(String),
    /// Timeout / unknown result: must reconcile, never blindly retry.
    Unknown,
}

#[async_trait]
pub trait ExecutionVenue: Send + Sync {
    fn name(&self) -> &str;

    /// Submit the order. Must be idempotent on `idempotency_key`.
    async fn submit(&self, order: &SwapOrder, idempotency_key: &str) -> VenueOutcome;

    /// Look up the state of a previously submitted order by idempotency key.
    async fn lookup(&self, order: &SwapOrder, idempotency_key: &str) -> VenueOutcome;
}

#[derive(Debug, Clone)]
pub struct ExecutionConfig {
    pub worker_id: String,
    pub batch_size: i64,
    /// Visibility timeout: a crashed worker's orders are reclaimed after this.
    pub lease_timeout: Duration,
    pub max_attempts: i32,
    pub base_backoff: Duration,
    pub max_backoff: Duration,
    pub poll_interval: Duration,
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            worker_id: format!("swap-worker-{}", Uuid::new_v4()),
            batch_size: 10,
            lease_timeout: Duration::from_secs(60),
            max_attempts: 5,
            base_backoff: Duration::from_secs(2),
            max_backoff: Duration::from_secs(60),
            poll_interval: Duration::from_secs(2),
        }
    }
}

/// Lease up to `batch_size` executable orders for this worker.
pub async fn lease_orders(pool: &PgPool, cfg: &ExecutionConfig) -> sqlx::Result<Vec<SwapOrder>> {
    sqlx::query_as::<_, SwapOrder>(
        "UPDATE swap_orders SET lease_owner = $1, \
             lease_expires_at = NOW() + make_interval(secs => $2), updated_at = NOW() \
         WHERE id IN ( \
             SELECT id FROM swap_orders \
             WHERE status = ANY($3) AND next_attempt_at <= NOW() \
               AND (lease_expires_at IS NULL OR lease_expires_at < NOW()) \
             ORDER BY next_attempt_at \
             LIMIT $4 \
             FOR UPDATE SKIP LOCKED) \
         RETURNING id, status, venue, source_asset, dest_asset, amount, filled_amount, attempts",
    )
    .bind(&cfg.worker_id)
    .bind(cfg.lease_timeout.as_secs_f64())
    .bind(
        EXECUTABLE_STATUSES
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>(),
    )
    .bind(cfg.batch_size)
    .fetch_all(pool)
    .await
}

/// Move an order between statuses. Validates against the domain guard, then
/// applies a conditional update (`WHERE status = from AND lease_owner = me`).
/// Returns `false` if the order changed underneath us or the lease was lost.
pub async fn transition_order(
    pool: &PgPool,
    cfg: &ExecutionConfig,
    order_id: Uuid,
    from: &str,
    to: &str,
    failure_code: Option<&str>,
) -> Result<bool, String> {
    validate_transition(from, to)?;
    let res = sqlx::query(
        "UPDATE swap_orders SET status = $1, failure_code = COALESCE($2, failure_code), \
             updated_at = NOW() \
         WHERE id = $3 AND status = $4 AND lease_owner = $5",
    )
    .bind(to)
    .bind(failure_code)
    .bind(order_id)
    .bind(from)
    .bind(&cfg.worker_id)
    .execute(pool)
    .await
    .map_err(|e| e.to_string())?;
    Ok(res.rows_affected() == 1)
}

/// Release the lease, bump the attempt counter and schedule the next attempt.
async fn release(
    pool: &PgPool,
    cfg: &ExecutionConfig,
    order: &SwapOrder,
) -> Result<(), String> {
    let delay = backoff(cfg, order.attempts);
    sqlx::query(
        "UPDATE swap_orders SET lease_owner = NULL, lease_expires_at = NULL, \
             attempts = attempts + 1, \
             next_attempt_at = NOW() + make_interval(secs => $1), updated_at = NOW() \
         WHERE id = $2 AND lease_owner = $3",
    )
    .bind(delay.as_secs_f64())
    .bind(order.id)
    .bind(&cfg.worker_id)
    .execute(pool)
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Clear the lease on an order that reached a terminal status.
async fn clear_lease(pool: &PgPool, order_id: Uuid) -> Result<(), String> {
    sqlx::query(
        "UPDATE swap_orders SET lease_owner = NULL, lease_expires_at = NULL WHERE id = $1",
    )
    .bind(order_id)
    .execute(pool)
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn backoff(cfg: &ExecutionConfig, attempts: i32) -> Duration {
    let factor = 1u32 << attempts.clamp(0, 16) as u32;
    cfg.base_backoff.saturating_mul(factor).min(cfg.max_backoff)
}

/// Record fills idempotently and return the reconciled total from the DB.
async fn record_fills(
    pool: &PgPool,
    order_id: Uuid,
    fills: &[VenueFill],
) -> sqlx::Result<BigDecimal> {
    for f in fills {
        sqlx::query(
            "INSERT INTO swap_fills (order_id, venue_fill_id, amount, price) \
             VALUES ($1, $2, $3, $4) ON CONFLICT (order_id, venue_fill_id) DO NOTHING",
        )
        .bind(order_id)
        .bind(&f.venue_fill_id)
        .bind(&f.amount)
        .bind(&f.price)
        .execute(pool)
        .await?;
    }
    let total: BigDecimal =
        sqlx::query("SELECT COALESCE(SUM(amount), 0) AS total FROM swap_fills WHERE order_id = $1")
            .bind(order_id)
            .fetch_one(pool)
            .await?
            .get("total");
    sqlx::query("UPDATE swap_orders SET filled_amount = $1 WHERE id = $2")
        .bind(&total)
        .bind(order_id)
        .execute(pool)
        .await?;
    Ok(total)
}

async fn fail_order(
    pool: &PgPool,
    cfg: &ExecutionConfig,
    order: &SwapOrder,
    from: &str,
    code: &str,
    reason: &str,
) -> Result<(), String> {
    if transition_order(pool, cfg, order.id, from, "failed", Some(code)).await? {
        let _ = record_event(
            pool,
            order.id,
            order.attempts + 1,
            SwapEventType::Failed,
            json!({ "code": code, "reason": reason }),
        )
        .await;
        clear_lease(pool, order.id).await?;
    }
    Ok(())
}

/// Drive one leased order through a single execution attempt.
pub async fn execute_order(
    pool: &PgPool,
    venue: &dyn ExecutionVenue,
    cfg: &ExecutionConfig,
    mut order: SwapOrder,
) -> Result<(), String> {
    let attempt = order.attempts + 1;
    let key = order.idempotency_key();

    if order.status == "quoted" {
        if !transition_order(pool, cfg, order.id, "quoted", "executing", None).await? {
            return Ok(()); // lost the race / lease
        }
        order.status = "executing".into();
    }
    let _ = record_event(
        pool,
        order.id,
        attempt,
        SwapEventType::Started,
        json!({ "venue": venue.name() }),
    )
    .await;

    let from = order.status.clone();
    let outcome = if from == "reconciling" {
        venue.lookup(&order, &key).await
    } else {
        let _ = record_event(
            pool,
            order.id,
            attempt,
            SwapEventType::Submitted,
            json!({ "idempotency_key": key }),
        )
        .await;
        venue.submit(&order, &key).await
    };

    match outcome {
        VenueOutcome::Filled(fills) | VenueOutcome::PartiallyFilled(fills) => {
            let total = record_fills(pool, order.id, &fills)
                .await
                .map_err(|e| e.to_string())?;
            if total >= order.amount {
                if transition_order(pool, cfg, order.id, &from, "filled", None).await? {
                    let _ = record_event(
                        pool,
                        order.id,
                        attempt,
                        SwapEventType::Filled,
                        json!({ "filled_amount": total.to_string() }),
                    )
                    .await;
                    clear_lease(pool, order.id).await?;
                }
                return Ok(());
            }
            if !transition_order(pool, cfg, order.id, &from, "partially_filled", None).await? {
                return Ok(());
            }
            let _ = record_event(
                pool,
                order.id,
                attempt,
                SwapEventType::PartiallyFilled,
                json!({ "filled_amount": total.to_string(), "amount": order.amount.to_string() }),
            )
            .await;
            if attempt >= cfg.max_attempts {
                fail_order(
                    pool,
                    cfg,
                    &order,
                    "partially_filled",
                    FAILURE_RETRIES_EXHAUSTED,
                    "partial fill not completed",
                )
                .await
            } else {
                release(pool, cfg, &order).await
            }
        }
        VenueOutcome::Transient(reason) => {
            if attempt >= cfg.max_attempts {
                fail_order(pool, cfg, &order, &from, FAILURE_RETRIES_EXHAUSTED, &reason).await
            } else {
                release(pool, cfg, &order).await
            }
        }
        VenueOutcome::Permanent(reason) => {
            fail_order(pool, cfg, &order, &from, FAILURE_VENUE_REJECTED, &reason).await
        }
        VenueOutcome::Unknown => {
            if from == "reconciling" {
                if attempt >= cfg.max_attempts {
                    fail_order(
                        pool,
                        cfg,
                        &order,
                        &from,
                        FAILURE_RECONCILIATION_FAILED,
                        "venue state unknown",
                    )
                    .await
                } else {
                    release(pool, cfg, &order).await
                }
            } else if transition_order(pool, cfg, order.id, &from, "reconciling", None).await? {
                release(pool, cfg, &order).await
            } else {
                Ok(())
            }
        }
    }
}

/// Lease and execute one batch. Returns the number of orders processed.
pub async fn run_once(
    pool: &PgPool,
    venues: &HashMap<String, Arc<dyn ExecutionVenue>>,
    cfg: &ExecutionConfig,
) -> Result<usize, String> {
    let orders = lease_orders(pool, cfg).await.map_err(|e| e.to_string())?;
    let n = orders.len();
    for order in orders {
        let result = match venues.get(&order.venue) {
            Some(v) => execute_order(pool, v.as_ref(), cfg, order.clone()).await,
            None => {
                // No venue registered on this replica: leave the order for
                // a replica that has it rather than failing it.
                tracing::error!(order_id = %order.id, venue = %order.venue, "no execution venue registered");
                release(pool, cfg, &order).await
            }
        };
        if let Err(e) = result {
            tracing::error!(order_id = %order.id, error = %e, "swap order execution error");
        }
    }
    Ok(n)
}

/// Worker loop; run one per replica.
pub async fn run_worker(
    pool: PgPool,
    venues: HashMap<String, Arc<dyn ExecutionVenue>>,
    cfg: ExecutionConfig,
) {
    loop {
        match run_once(&pool, &venues, &cfg).await {
            Ok(0) => tokio::time::sleep(cfg.poll_interval).await,
            Ok(_) => {}
            Err(e) => {
                tracing::error!(error = %e, "swap execution batch failed");
                tokio::time::sleep(cfg.poll_interval).await;
            }
        }
    }
}
