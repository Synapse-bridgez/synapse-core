//! Versioned lifecycle event contract published through the transactional
//! outbox (see [`crate::db::outbox`]).
//!
//! # Envelope contract (stable)
//!
//! Every event is delivered as an [`EventEnvelope`] serialized to JSON:
//!
//! | Field         | Type             | Meaning                                                              |
//! |---------------|------------------|----------------------------------------------------------------------|
//! | `event_id`    | UUID             | Dedupe key. Stable across publish retries; consumers must dedupe on it. |
//! | `order_id`    | UUID             | The order (transaction) the event belongs to.                        |
//! | `sequence`    | integer >= 1     | Per-order monotonic sequence. Apply events for an order in ascending order. |
//! | `event_type`  | string           | One of the `EVENT_*` constants in this module.                        |
//! | `version`     | integer          | Envelope/payload schema version ([`EVENT_ENVELOPE_VERSION`]).         |
//! | `tenant_id`   | UUID or null     | Owning tenant; `null` for unattributed transactions.                 |
//! | `occurred_at` | RFC 3339 instant | When the domain state change committed.                              |
//! | `payload`     | JSON object      | Event-type-specific data. Carries no PII beyond what consumers need. |
//!
//! Compatibility rules: fields are only added, never removed or renamed. A
//! breaking payload change bumps `version` and keeps emitting the old version
//! until consumers have moved.
//!
//! Delivery is at-least-once. Consumers must tolerate duplicates (dedupe on
//! `event_id`) and must not assume cross-order ordering; only `sequence`
//! within one `order_id` is ordered.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Current envelope and payload schema version.
pub const EVENT_ENVELOPE_VERSION: i32 = 1;

/// A transaction row was created.
pub const EVENT_TRANSACTION_CREATED: &str = "transaction.created";

/// A transaction's `status` changed. `payload.status` holds the new status.
pub const EVENT_TRANSACTION_STATUS_CHANGED: &str = "transaction.status_changed";

/// The stable on-the-wire event envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub event_id: Uuid,
    pub order_id: Uuid,
    pub sequence: i64,
    pub event_type: String,
    pub version: i32,
    pub tenant_id: Option<Uuid>,
    pub occurred_at: DateTime<Utc>,
    pub payload: serde_json::Value,
}
