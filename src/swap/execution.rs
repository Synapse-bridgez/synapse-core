//! Phase 2 swap order execution engine.
//!
//! Drives a `quoted` swap order to `settled` or `failed` by calling a
//! [`SwapVenue`] and persisting a [`SwapExecutionRecord`] linked to the order.
//!
//! The venue call sits between two short database writes and is never made
//! inside a transaction:
//!
//! 1. claim the order (`quoted` -> `executing`), which rejects concurrent
//!    attempts on the same order;
//! 2. call the venue with the order's idempotency key, retrying transient
//!    failures with that same key;
//! 3. write the terminal status and execution record in one transaction.
//!
//! If the process dies between steps 1 and 3 the order stays `executing`.
//! [`requeue_stale_orders`] returns such orders to `quoted`, and the retry
//! reuses the same idempotency key, so a venue that dedupes on it returns the
//! original fill instead of executing twice.

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::types::BigDecimal;
use sqlx::PgPool;
use uuid::Uuid;

use crate::db::queries;
use crate::error::AppError;

/// Total venue calls allowed per order (first attempt plus retries).
pub const MAX_VENUE_ATTEMPTS: u32 = 3;
/// Backoff before the first retry; doubles on each subsequent retry.
const RETRY_BASE_DELAY: Duration = Duration::from_millis(250);

pub const STATUS_SETTLED: &str = "settled";
pub const STATUS_FAILED: &str = "failed";

/// What the engine asks a venue to execute.
///
/// Venues must treat `idempotency_key` as a dedupe key: resubmitting the same
/// key returns the original fill rather than executing again.
#[derive(Debug, Clone)]
pub struct VenueRequest {
    pub order_id: Uuid,
    pub idempotency_key: String,
    pub source_asset: String,
    pub target_asset: String,
    pub amount: BigDecimal,
    pub minimum_output: BigDecimal,
}

/// A venue's report of what it actually executed.
#[derive(Debug, Clone, PartialEq)]
pub struct VenueFill {
    pub executed_amount_in: BigDecimal,
    pub executed_amount_out: BigDecimal,
    pub fee: BigDecimal,
    pub venue_reference: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VenueError {
    /// Timeout, rate limit, or temporary outage. Retried with the same
    /// idempotency key. A timeout is ambiguous (the venue may already have
    /// executed), which is why the key must be honoured.
    Transient(String),
    /// The venue refused the order. Not retried.
    Rejected(String),
}

/// A liquidity venue that can execute a swap.
#[async_trait]
pub trait SwapVenue: Send + Sync {
    /// Short identifier stored on the execution record (e.g. `"mock"`).
    fn name(&self) -> &str;

    /// Execute the request. Implementations should enforce
    /// `minimum_output` as a limit; the engine re-checks the fill regardless.
    async fn execute(&self, req: &VenueRequest) -> Result<VenueFill, VenueError>;
}

/// Why an order ended `failed`. Persisted as its [`as_str`](Self::as_str) code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapFailureReason {
    /// The realized output was below the quoted minimum.
    SlippageBreach,
    /// The venue rejected the order.
    VenueRejected,
    /// Transient venue failures persisted through every attempt.
    RetriesExhausted,
}

impl SwapFailureReason {
    pub fn as_str(self) -> &'static str {
        match self {
            SwapFailureReason::SlippageBreach => "slippage_breach",
            SwapFailureReason::VenueRejected => "venue_rejected",
            SwapFailureReason::RetriesExhausted => "retries_exhausted",
        }
    }
}

/// The execution record persisted for a swap order. There is at most one per
/// order.
#[derive(Debug, Clone)]
pub struct SwapExecutionRecord {
    pub order_id: Uuid,
    pub idempotency_key: String,
    pub venue: String,
    pub executed_amount_in: Option<BigDecimal>,
    pub executed_amount_out: Option<BigDecimal>,
    pub fee: Option<BigDecimal>,
    pub venue_reference: Option<String>,
    pub attempts: i32,
    pub failure_reason: Option<SwapFailureReason>,
    pub failure_detail: Option<String>,
    pub started_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
}

#[derive(Debug, thiserror::Error)]
pub enum SwapExecutionError {
    #[error("swap order {0} is not in a quoted state")]
    NotQuoted(Uuid),
    #[error("swap order {0} changed state during execution")]
    StateChanged(Uuid),
    #[error("swap order persistence failed: {0}")]
    Database(#[from] sqlx::Error),
}

impl From<SwapExecutionError> for AppError {
    fn from(err: SwapExecutionError) -> Self {
        match err {
            SwapExecutionError::NotQuoted(id) => AppError::SwapOrderNotQuoted(id.to_string()),
            SwapExecutionError::StateChanged(id) => {
                AppError::ConcurrentModification(id.to_string())
            }
            SwapExecutionError::Database(err) => AppError::Database(err),
        }
    }
}

/// Executes one quoted swap order against `venue` and returns the persisted
/// record.
///
/// A `failed` outcome (slippage breach, venue rejection, or exhausted retries)
/// is still `Ok`, with `failure_reason` set. `Err` means the order could not be
/// claimed, or its outcome could not be persisted. In the latter case the order
/// stays `executing` and [`requeue_stale_orders`] will pick it up.
///
/// A partial fill settles with the amounts actually executed, provided the
/// realized output still meets `minimum_output`.
pub async fn execute_order(
    pool: &PgPool,
    venue: &dyn SwapVenue,
    order_id: Uuid,
) -> Result<SwapExecutionRecord, SwapExecutionError> {
    let order = queries::claim_quoted_swap_order(pool, order_id)
        .await?
        .ok_or(SwapExecutionError::NotQuoted(order_id))?;

    let started_at = Utc::now();
    let request = VenueRequest {
        order_id,
        idempotency_key: format!("swap:{order_id}"),
        source_asset: order.source_asset.clone(),
        target_asset: order.target_asset.clone(),
        amount: order.amount.clone(),
        minimum_output: order.minimum_output.clone(),
    };

    let (attempts, outcome) = call_venue(venue, &request).await;
    let mut record = SwapExecutionRecord {
        order_id,
        idempotency_key: request.idempotency_key.clone(),
        venue: venue.name().to_string(),
        executed_amount_in: None,
        executed_amount_out: None,
        fee: None,
        venue_reference: None,
        attempts: attempts as i32,
        failure_reason: None,
        failure_detail: None,
        started_at,
        completed_at: Utc::now(),
    };

    let status = match outcome {
        Ok(fill) => {
            let below_minimum = fill.executed_amount_out < order.minimum_output;
            record.failure_detail = below_minimum.then(|| {
                format!(
                    "realized output {} below minimum {}",
                    fill.executed_amount_out, order.minimum_output
                )
            });
            record.executed_amount_in = Some(fill.executed_amount_in);
            record.executed_amount_out = Some(fill.executed_amount_out);
            record.fee = Some(fill.fee);
            record.venue_reference = Some(fill.venue_reference);
            if below_minimum {
                record.failure_reason = Some(SwapFailureReason::SlippageBreach);
                STATUS_FAILED
            } else {
                STATUS_SETTLED
            }
        }
        Err((reason, detail)) => {
            record.failure_reason = Some(reason);
            record.failure_detail = Some(detail);
            STATUS_FAILED
        }
    };

    if !queries::finish_swap_order(pool, status, &record).await? {
        return Err(SwapExecutionError::StateChanged(order_id));
    }

    Ok(record)
}

/// Returns swap orders stuck in `executing` since before `older_than` to
/// `quoted`, so [`execute_order`] can drive them again.
///
/// Choose `older_than` well beyond the venue timeout times the retry budget.
/// If it is too short, an order still in flight can be re-driven concurrently.
/// The shared idempotency key and the guarded finish still ensure only one
/// outcome is persisted.
pub async fn requeue_stale_orders(
    pool: &PgPool,
    older_than: DateTime<Utc>,
) -> Result<u64, SwapExecutionError> {
    Ok(queries::requeue_stale_swap_orders(pool, older_than).await?)
}

/// Calls the venue, retrying transient failures with exponential backoff.
/// Returns the number of venue calls made alongside the outcome.
async fn call_venue(
    venue: &dyn SwapVenue,
    request: &VenueRequest,
) -> (u32, Result<VenueFill, (SwapFailureReason, String)>) {
    let mut attempts = 0;
    loop {
        attempts += 1;
        match venue.execute(request).await {
            Ok(fill) => return (attempts, Ok(fill)),
            Err(VenueError::Rejected(detail)) => {
                return (attempts, Err((SwapFailureReason::VenueRejected, detail)))
            }
            Err(VenueError::Transient(detail)) if attempts >= MAX_VENUE_ATTEMPTS => {
                return (attempts, Err((SwapFailureReason::RetriesExhausted, detail)))
            }
            Err(VenueError::Transient(detail)) => {
                tracing::warn!(
                    order_id = %request.order_id,
                    attempt = attempts,
                    error = %detail,
                    "transient swap venue failure, retrying with same idempotency key"
                );
                tokio::time::sleep(RETRY_BASE_DELAY * 2u32.pow(attempts - 1)).await;
            }
        }
    }
}
