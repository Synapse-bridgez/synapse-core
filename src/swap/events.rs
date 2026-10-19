//! Transactional outbox dispatcher for swap lifecycle events (Issue #1437).
//!
//! Lifecycle events are written to `event_outbox` in the same DB transaction as
//! the state change (see [`queries::update_transaction_status_versioned`]).
//! [`dispatch_pending_events`] publishes them and marks each row dispatched only
//! after its publish succeeds. A crash between publish and mark re-publishes the
//! event on the next tick, so delivery is at-least-once. Consumers must dedupe
//! on [`OutboxEvent::id`].

use async_trait::async_trait;
use sqlx::PgPool;

use crate::db::queries::{self, OutboxEvent};

/// Publishes one outbox event to a downstream transport (broker, webhook, etc.).
#[async_trait]
pub trait EventPublisher: Send + Sync {
    async fn publish(&self, event: &OutboxEvent) -> Result<(), String>;
}

/// Publishes up to `batch_size` pending events in id order and returns how many
/// were dispatched. Stops at the first publish failure so later events are not
/// delivered ahead of the failed one. The failed row stays pending for the next
/// tick.
pub async fn dispatch_pending_events(
    pool: &PgPool,
    publisher: &dyn EventPublisher,
    batch_size: i64,
) -> sqlx::Result<usize> {
    let events = queries::fetch_pending_outbox_events(pool, batch_size).await?;
    let mut dispatched = 0;

    for event in &events {
        if let Err(e) = publisher.publish(event).await {
            tracing::warn!(
                outbox_id = event.id,
                event_type = %event.event_type,
                error = %e,
                "outbox publish failed; will retry on next dispatch"
            );
            break;
        }
        queries::mark_outbox_event_dispatched(pool, event.id).await?;
        dispatched += 1;
    }

    Ok(dispatched)
}
