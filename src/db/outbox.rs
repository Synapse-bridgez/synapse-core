//! # Transactional Outbox
//!
//! Lifecycle events are written to `outbox_events` in the same SQL transaction
//! as the domain state change, so an event exists if and only if the change
//! committed. [`run_outbox_worker`] drains those rows through an
//! [`EventPublisher`].
//!
//! ## Delivery guarantees
//!
//! - **At-least-once**: a row is marked published in the same DB transaction
//!   that claimed it, and only after `publish` returns `Ok`. A crash between
//!   publish and commit re-delivers the event with the same `event_id`.
//! - **Per-order ordering**: only the lowest unpublished `sequence` of an order
//!   is eligible, so a later event for an order is never published before an
//!   earlier one. A dead-lettered or backing-off event therefore holds back
//!   later events of the same order until it resolves.
//! - **Backpressure**: a failed publish ends the batch. The failing row is
//!   retried with capped exponential backoff via `next_attempt_at`.
//! - **Dead-lettering**: after [`OUTBOX_MAX_ATTEMPTS`] failures the row gets
//!   `dead_lettered_at` and an error-level log line with
//!   `counter.outbox_dead_lettered_total`, which log-based alerting can match.
//!
//! ## Consumer contract
//!
//! The envelope is defined in [`crate::swap::events`]. Consumers dedupe on
//! `event_id` and apply each order's events in `sequence` order.

use crate::swap::events::{EventEnvelope, EVENT_ENVELOPE_VERSION};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Transaction as SqlxTransaction};
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::watch;
use uuid::Uuid;

/// Maximum number of due events claimed per drain.
pub const OUTBOX_BATCH_SIZE: i64 = 100;

/// Failed publish attempts before an event is dead-lettered.
pub const OUTBOX_MAX_ATTEMPTS: i32 = 20;

/// Published events older than this are pruned.
pub const OUTBOX_RETENTION_DAYS: i32 = 30;

const POLL_INTERVAL: Duration = Duration::from_secs(1);
const PRUNE_INTERVAL: Duration = Duration::from_secs(3600);
const MAX_BACKOFF_SECS: u64 = 300;

/// Error returned by an [`EventPublisher`]. Every error is retried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishError(pub String);

impl fmt::Display for PublishError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "event publish failed: {}", self.0)
    }
}

impl std::error::Error for PublishError {}

/// Destination for outbox events: a broker, a webhook, or a log.
///
/// `publish` must return `Ok` only after the destination has durably accepted
/// the event. Implementations should use `envelope.event_id` as the dedupe key
/// on their side.
#[async_trait]
pub trait EventPublisher: Send + Sync {
    /// Short identifier for logs (e.g. `"log"`, `"kafka"`).
    fn name(&self) -> &str;

    /// Publish one event envelope.
    async fn publish(&self, envelope: &EventEnvelope) -> Result<(), PublishError>;
}

/// Local-dev sink that logs each event instead of sending it anywhere.
#[derive(Debug, Default, Clone, Copy)]
pub struct LogEventPublisher;

#[async_trait]
impl EventPublisher for LogEventPublisher {
    fn name(&self) -> &str {
        "log"
    }

    async fn publish(&self, envelope: &EventEnvelope) -> Result<(), PublishError> {
        tracing::info!(
            event_id = %envelope.event_id,
            order_id = %envelope.order_id,
            sequence = envelope.sequence,
            event_type = %envelope.event_type,
            version = envelope.version,
            "Outbox event published (log sink)"
        );
        Ok(())
    }
}

/// Appends an event for `transaction_id` to the outbox inside `db_tx`.
///
/// Call this in the same SQL transaction as the state change, after the change,
/// so the payload reads the post-change row. The per-order sequence comes from
/// `outbox_sequences` under a row lock, so concurrent writers on one order are
/// serialized instead of racing on `MAX(sequence) + 1`.
pub async fn enqueue_transaction_event(
    db_tx: &mut SqlxTransaction<'_, Postgres>,
    transaction_id: Uuid,
    event_type: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        r#"
        WITH next_seq AS (
            INSERT INTO outbox_sequences (aggregate_id, last_sequence)
            VALUES ($1, 1)
            ON CONFLICT (aggregate_id)
            DO UPDATE SET last_sequence = outbox_sequences.last_sequence + 1
            RETURNING last_sequence
        )
        INSERT INTO outbox_events (aggregate_id, sequence, event_type, version, tenant_id, payload)
        SELECT t.id, next_seq.last_sequence, $2, $3, t.tenant_id,
               jsonb_build_object('status', t.status, 'asset_code', t.asset_code, 'amount', t.amount::text)
        FROM transactions t, next_seq
        WHERE t.id = $1
        "#,
    )
    .bind(transaction_id)
    .bind(event_type)
    .bind(EVENT_ENVELOPE_VERSION)
    .execute(&mut **db_tx)
    .await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct OutboxRow {
    event_id: Uuid,
    aggregate_id: Uuid,
    sequence: i64,
    event_type: String,
    version: i32,
    tenant_id: Option<Uuid>,
    occurred_at: DateTime<Utc>,
    payload: serde_json::Value,
    attempts: i32,
}

/// Claims up to `batch_size` due events, publishes them, and records each
/// outcome. Stops at the first failed publish. Returns the number of events
/// published.
///
/// The claim uses `FOR UPDATE SKIP LOCKED`, so concurrent workers take disjoint
/// rows. The row locks are held for the duration of the batch.
pub async fn drain_batch(
    pool: &PgPool,
    publisher: &dyn EventPublisher,
    batch_size: i64,
) -> sqlx::Result<usize> {
    let mut db_tx = pool.begin().await?;

    // Only the lowest unpublished sequence of each order is eligible, which
    // keeps per-order delivery in sequence order.
    let rows = sqlx::query_as::<_, OutboxRow>(
        r#"
        SELECT o.event_id, o.aggregate_id, o.sequence, o.event_type, o.version,
               o.tenant_id, o.occurred_at, o.payload, o.attempts
        FROM outbox_events o
        WHERE o.published_at IS NULL
          AND o.dead_lettered_at IS NULL
          AND o.next_attempt_at <= NOW()
          AND NOT EXISTS (
              SELECT 1 FROM outbox_events prev
              WHERE prev.aggregate_id = o.aggregate_id
                AND prev.sequence < o.sequence
                AND prev.published_at IS NULL
          )
        ORDER BY o.occurred_at, o.sequence
        LIMIT $1
        FOR UPDATE OF o SKIP LOCKED
        "#,
    )
    .bind(batch_size)
    .fetch_all(&mut *db_tx)
    .await?;

    let mut published = 0;
    for row in rows {
        let attempts = row.attempts;
        let envelope = EventEnvelope {
            event_id: row.event_id,
            order_id: row.aggregate_id,
            sequence: row.sequence,
            event_type: row.event_type,
            version: row.version,
            tenant_id: row.tenant_id,
            occurred_at: row.occurred_at,
            payload: row.payload,
        };

        match publisher.publish(&envelope).await {
            Ok(()) => {
                sqlx::query(
                    "UPDATE outbox_events SET published_at = NOW(), last_error = NULL WHERE event_id = $1",
                )
                .bind(envelope.event_id)
                .execute(&mut *db_tx)
                .await?;
                published += 1;
            }
            Err(err) => {
                let attempts = attempts + 1;
                let dead = attempts >= OUTBOX_MAX_ATTEMPTS;
                let backoff_secs = ((1u64 << attempts.min(16)).min(MAX_BACKOFF_SECS)) as f64;
                sqlx::query(
                    r#"
                    UPDATE outbox_events
                    SET attempts = $2,
                        last_error = $3,
                        next_attempt_at = NOW() + make_interval(secs => $4),
                        dead_lettered_at = CASE WHEN $5 THEN NOW() ELSE NULL END
                    WHERE event_id = $1
                    "#,
                )
                .bind(envelope.event_id)
                .bind(attempts)
                .bind(err.to_string())
                .bind(backoff_secs)
                .bind(dead)
                .execute(&mut *db_tx)
                .await?;

                if dead {
                    tracing::error!(
                        counter.outbox_dead_lettered_total = 1u64,
                        event_id = %envelope.event_id,
                        order_id = %envelope.order_id,
                        sequence = envelope.sequence,
                        attempts,
                        error = %err,
                        "Outbox event dead-lettered; its order is blocked until it is resolved"
                    );
                } else {
                    tracing::warn!(
                        event_id = %envelope.event_id,
                        order_id = %envelope.order_id,
                        attempts,
                        backoff_secs,
                        error = %err,
                        "Outbox publish failed; will retry"
                    );
                }
                break;
            }
        }
    }

    db_tx.commit().await?;
    Ok(published)
}

/// Deletes published events older than `retention_days`. Unpublished and
/// dead-lettered rows are never pruned. `outbox_sequences` is never pruned
/// either, so an order's sequence cannot reset.
pub async fn prune_published(pool: &PgPool, retention_days: i32) -> sqlx::Result<u64> {
    let result = sqlx::query(
        "DELETE FROM outbox_events WHERE published_at < NOW() - make_interval(days => $1)",
    )
    .bind(retention_days)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Background loop that drains the outbox until `shutdown` flips to `true`.
///
/// When a batch comes back full, the next drain runs without waiting for the
/// poll interval, so a backlog drains at full speed.
pub async fn run_outbox_worker(
    pool: PgPool,
    publisher: Arc<dyn EventPublisher>,
    mut shutdown: watch::Receiver<bool>,
) {
    tracing::info!(publisher = publisher.name(), "Outbox worker started");

    let mut poll = tokio::time::interval(POLL_INTERVAL);
    let mut last_prune = Instant::now();
    let mut backlog = false;

    loop {
        if !backlog {
            tokio::select! {
                _ = poll.tick() => {}
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
            }
        }

        backlog = match drain_batch(&pool, publisher.as_ref(), OUTBOX_BATCH_SIZE).await {
            Ok(published) => published as i64 >= OUTBOX_BATCH_SIZE,
            Err(err) => {
                tracing::error!(error = %err, "Outbox drain failed");
                false
            }
        };

        if last_prune.elapsed() >= PRUNE_INTERVAL {
            last_prune = Instant::now();
            match prune_published(&pool, OUTBOX_RETENTION_DAYS).await {
                Ok(0) => {}
                Ok(pruned) => tracing::info!(pruned, "Pruned published outbox events"),
                Err(err) => tracing::error!(error = %err, "Outbox prune failed"),
            }
        }
    }

    tracing::info!("Outbox worker stopped");
}
