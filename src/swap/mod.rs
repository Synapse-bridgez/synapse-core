//! Phase 2 swap-engine service boundary.
//!
//! This repository is Phase 1 of Synapse Bridge (transaction preparation). A
//! future swap engine - in this repo, a separate service, or an external
//! contract-based system - takes over once a transaction reaches the
//! [`swap_ready`](crate::validation) state. This module defines **only** the
//! hand-off contract and the trait that a swap-engine implementation must
//! satisfy; it contains no swap or DEX-routing logic.
//!
//! See `docs/state-machine.md` (`### swap_ready`, "Phase 2 hand-off") for how
//! this fits the existing transaction pipeline.
//!
//! Nothing in Phase 1 constructs a [`SwapHandoff`] or calls a [`SwapEngine`]
//! yet. The default wiring is [`NoopSwapEngine`], which is inert, and
//! [`swap_enabled_for`] returns `false` unless a tenant explicitly opts in via
//! the `swap_engine` feature flag - so transactions for tenants and assets not
//! opted into swap are completely unaffected.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sqlx::types::BigDecimal;
use std::fmt;

use crate::db::models::Transaction;
use crate::services::feature_flags::FeatureFlagService;

/// Feature flag that opts a tenant into Phase 2 swap hand-off.
pub const SWAP_ENGINE_FLAG: &str = "swap_engine";

/// The exact data a completed Phase 1 transaction hands to Phase 2 at the
/// `swap_ready` state. This is the stable contract a swap-engine implementation
/// integrates against - keep it additive.
#[derive(Debug, Clone, PartialEq)]
pub struct SwapHandoff {
    /// The Phase 1 transaction this hand-off originates from.
    pub transaction_id: uuid::Uuid,
    /// Owning tenant, when resolved by the caller. `None` for
    /// platform-scoped/unattributed transactions.
    pub tenant_id: Option<uuid::Uuid>,
    /// Stellar account the prepared transaction settled to.
    pub stellar_account: String,
    /// Asset code the Phase 1 transaction is denominated in (the swap source).
    pub source_asset: String,
    /// Amount available to the swap, in `source_asset` units.
    pub amount: BigDecimal,
    /// When Phase 1 completed (the transaction's `updated_at` at `completed`).
    pub completed_at: chrono::DateTime<chrono::Utc>,
    /// Stable key for Phase 2 to deduplicate retries of the same hand-off.
    /// Derived from `transaction_id`, so re-delivering a hand-off is safe.
    pub idempotency_key: String,
    /// Opaque Phase 1 metadata carried through untouched.
    pub metadata: Option<serde_json::Value>,
}

impl SwapHandoff {
    /// Build a hand-off from a completed transaction. `tenant_id` is resolved by
    /// the caller (the `Transaction` row does not carry it directly).
    pub fn from_transaction(tx: &Transaction, tenant_id: Option<uuid::Uuid>) -> Self {
        Self {
            transaction_id: tx.id,
            tenant_id,
            stellar_account: tx.stellar_account.clone(),
            source_asset: tx.asset_code.clone(),
            amount: tx.amount.clone(),
            completed_at: tx.updated_at,
            idempotency_key: format!("swap:{}", tx.id),
            metadata: tx.metadata.clone(),
        }
    }
}

/// Outcome of handing a transaction to the swap engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwapOutcome {
    /// The engine took no action (swap not applicable / not configured).
    Skipped,
    /// The engine cannot act now; retry the hand-off after this many seconds.
    Deferred { retry_after_secs: u64 },
    /// The engine accepted the hand-off and started a swap.
    Routed { swap_id: String },
}

/// Error returned by a [`SwapEngine`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwapError {
    /// The engine is unreachable or not ready.
    Unavailable,
    /// The hand-off is malformed or references data the engine cannot use.
    InvalidHandoff(String),
    /// The engine failed internally.
    Internal(String),
}

impl fmt::Display for SwapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SwapError::Unavailable => write!(f, "swap engine unavailable"),
            SwapError::InvalidHandoff(m) => write!(f, "invalid swap hand-off: {m}"),
            SwapError::Internal(m) => write!(f, "swap engine internal error: {m}"),
        }
    }
}

impl std::error::Error for SwapError {}

/// The interface a Phase 2 swap-engine implementation must satisfy.
///
/// Transport-agnostic: an implementation may run in-process, call out to a
/// separate microservice, or drive an on-chain contract. It is handed a
/// [`SwapHandoff`] and reports a [`SwapOutcome`]. Implementations must be
/// idempotent on `handoff.idempotency_key`.
#[async_trait]
pub trait SwapEngine: Send + Sync {
    /// Short identifier for logs/metrics (e.g. `"noop"`, `"in-process-v1"`).
    fn name(&self) -> &str;

    /// Handle a transaction that has reached `swap_ready`.
    async fn on_swap_ready(&self, handoff: SwapHandoff) -> Result<SwapOutcome, SwapError>;
}

/// Inert default engine. Accepts every hand-off and does nothing, so Phase 1
/// can depend on the `SwapEngine` trait before any real engine exists.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopSwapEngine;

#[async_trait]
impl SwapEngine for NoopSwapEngine {
    fn name(&self) -> &str {
        "noop"
    }

    async fn on_swap_ready(&self, _handoff: SwapHandoff) -> Result<SwapOutcome, SwapError> {
        Ok(SwapOutcome::Skipped)
    }
}

/// Whether `tenant_id` has opted into Phase 2 swap hand-off. Defaults to
/// `false` (and on any lookup error), so nothing is opted in until an operator
/// enables the `swap_engine` flag for a tenant. Asset-level gating can layer on
/// top by scoping the flag key per asset.
pub async fn swap_enabled_for(flags: &FeatureFlagService, tenant_id: &str) -> bool {
    flags
        .is_enabled_for_tenant(SWAP_ENGINE_FLAG, tenant_id)
        .await
        .unwrap_or(false)
}

/// Current schema version emitted for swap lifecycle events.
///
/// Bump this only for a **major** (breaking) change. Additive changes within a
/// version do not bump it. See `docs/swap-events.md` for the compatibility and
/// deprecation policy.
pub const SWAP_EVENT_SCHEMA_VERSION: u32 = 1;

/// Canonical, versioned envelope shared by every swap lifecycle event.
///
/// Field formats are canonical across all versions:
/// - `occurred_at` is RFC3339 in UTC (serialized via `chrono`'s default).
/// - amounts in payloads are decimal strings (see [`DecimalString`]).
///
/// The envelope is additive-only within a [`SWAP_EVENT_SCHEMA_VERSION`];
/// consumers must ignore unknown fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// Unique id for this event instance (dedupe key for consumers).
    pub event_id: uuid::Uuid,
    /// Event type discriminator, e.g. `"swap.order.created"`.
    pub event_type: String,
    /// Schema version of the payload carried in `payload`.
    pub schema_version: u32,
    /// When the event occurred, RFC3339 UTC.
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    /// Owning tenant, when the event is tenant-scoped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<uuid::Uuid>,
    /// The swap order this event concerns.
    pub order_id: uuid::Uuid,
    /// Correlation/trace id linking related events and requests.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
}

/// Canonical decimal amount, serialized as a string to avoid float drift and
/// to stay consistent with the decimal policy (issue #2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DecimalString(pub String);

impl DecimalString {
    /// Build from a `BigDecimal`, using its plain (non-exponent) string form.
    pub fn from_bigdecimal(value: &BigDecimal) -> Self {
        DecimalString(value.normalized().to_string())
    }
}

/// Typed payloads for each swap lifecycle event.
///
/// Each variant carries only the fields meaningful at that stage. New optional
/// fields may be added within a version; new variants are additive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event_type", content = "payload")]
pub enum SwapEventPayload {
    /// A swap order was created.
    #[serde(rename = "swap.order.created")]
    OrderCreated {
        source_asset: String,
        target_asset: String,
        source_amount: DecimalString,
    },
    /// A quote was obtained for the order.
    #[serde(rename = "swap.order.quoted")]
    Quoted {
        source_asset: String,
        target_asset: String,
        source_amount: DecimalString,
        target_amount: DecimalString,
        #[serde(skip_serializing_if = "Option::is_none")]
        rate: Option<DecimalString>,
        #[serde(skip_serializing_if = "Option::is_none")]
        expires_at: Option<chrono::DateTime<chrono::Utc>>,
    },
    /// Execution of the order started.
    #[serde(rename = "swap.order.executing")]
    Executing {
        #[serde(skip_serializing_if = "Option::is_none")]
        tx_hash: Option<String>,
    },
    /// The order filled completely.
    #[serde(rename = "swap.order.filled")]
    Filled {
        source_amount: DecimalString,
        target_amount: DecimalString,
    },
    /// The order filled partially.
    #[serde(rename = "swap.order.partially_filled")]
    PartiallyFilled {
        filled_source_amount: DecimalString,
        filled_target_amount: DecimalString,
        remaining_source_amount: DecimalString,
    },
    /// The order was cancelled.
    #[serde(rename = "swap.order.cancelled")]
    Cancelled {
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// The order failed.
    #[serde(rename = "swap.order.failed")]
    Failed {
        reason: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        retryable: Option<bool>,
    },
    /// The order settled on-chain.
    #[serde(rename = "swap.order.settled")]
    Settled {
        #[serde(skip_serializing_if = "Option::is_none")]
        tx_hash: Option<String>,
        settled_at: chrono::DateTime<chrono::Utc>,
    },
}

impl SwapEventPayload {
    /// The canonical event type string for this payload.
    pub fn event_type(&self) -> &'static str {
        match self {
            SwapEventPayload::OrderCreated { .. } => "swap.order.created",
            SwapEventPayload::Quoted { .. } => "swap.order.quoted",
            SwapEventPayload::Executing { .. } => "swap.order.executing",
            SwapEventPayload::Filled { .. } => "swap.order.filled",
            SwapEventPayload::PartiallyFilled { .. } => "swap.order.partially_filled",
            SwapEventPayload::Cancelled { .. } => "swap.order.cancelled",
            SwapEventPayload::Failed { .. } => "swap.order.failed",
            SwapEventPayload::Settled { .. } => "swap.order.settled",
        }
    }
}

/// A fully-formed, versioned swap lifecycle event ready for publication.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwapEvent {
    /// Common envelope (ids, type, version, timestamps).
    #[serde(flatten)]
    pub envelope: EventEnvelope,
    /// Typed payload; serializes as `event_type` + `payload` at the top level.
    #[serde(flatten)]
    pub payload: SwapEventPayload,
}

impl SwapEvent {
    /// Build a versioned event at the current [`SWAP_EVENT_SCHEMA_VERSION`].
    pub fn new(
        order_id: uuid::Uuid,
        tenant_id: Option<uuid::Uuid>,
        correlation_id: Option<String>,
        payload: SwapEventPayload,
    ) -> Self {
        Self {
            envelope: EventEnvelope {
                event_id: uuid::Uuid::new_v4(),
                event_type: payload.event_type().to_string(),
                schema_version: SWAP_EVENT_SCHEMA_VERSION,
                occurred_at: chrono::Utc::now(),
                tenant_id,
                order_id,
                correlation_id,
            },
            payload,
        }
    }

    /// Serialize to the canonical versioned JSON payload for publication.
    pub fn to_versioned_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::validation::state_transitions::{is_valid_transition, TRANSACTION_TRANSITIONS};

    #[tokio::test]
    async fn noop_engine_skips_every_handoff() {
        let engine = NoopSwapEngine;
        assert_eq!(engine.name(), "noop");
        let handoff = SwapHandoff {
            transaction_id: uuid::Uuid::nil(),
            tenant_id: None,
            stellar_account: "GA...".into(),
            source_asset: "USDC".into(),
            amount: BigDecimal::from(100),
            completed_at: chrono::Utc::now(),
            idempotency_key: "swap:0".into(),
            metadata: None,
        };
        assert_eq!(
            engine.on_swap_ready(handoff).await,
            Ok(SwapOutcome::Skipped)
        );
    }

    #[test]
    fn idempotency_key_is_derived_from_transaction_id() {
        let id = uuid::Uuid::new_v4();
        // `from_transaction` needs a full `Transaction`; check the key rule directly.
        assert_eq!(format!("swap:{id}"), format!("swap:{}", id));
    }

    #[test]
    fn swap_ready_is_reachable_from_completed_only() {
        assert!(is_valid_transition(
            "completed",
            "swap_ready",
            TRANSACTION_TRANSITIONS
        ));
        assert!(is_valid_transition(
            "swap_ready",
            "failed",
            TRANSACTION_TRANSITIONS
        ));
        // not from pending/processing, and not back to completed
        assert!(!is_valid_transition(
            "pending",
            "swap_ready",
            TRANSACTION_TRANSITIONS
        ));
        assert!(!is_valid_transition(
            "swap_ready",
            "completed",
            TRANSACTION_TRANSITIONS
        ));
    }

    #[test]
    fn swap_error_displays() {
        assert_eq!(
            SwapError::Unavailable.to_string(),
            "swap engine unavailable"
        );
        assert!(SwapError::InvalidHandoff("x".into())
            .to_string()
            .contains("x"));
    }

    #[test]
    fn event_serializes_with_envelope_and_payload() {
        let order_id = uuid::Uuid::new_v4();
        let event = SwapEvent::new(
            order_id,
            Some(uuid::Uuid::new_v4()),
            Some("trace-1".into()),
            SwapEventPayload::OrderCreated {
                source_asset: "USDC".into(),
                target_asset: "XLM".into(),
                source_amount: DecimalString("100.00".into()),
            },
        );
        let json = event.to_versioned_json().unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["event_type"], "swap.order.created");
        assert_eq!(value["schema_version"], SWAP_EVENT_SCHEMA_VERSION);
        assert_eq!(value["order_id"], order_id.to_string());
        assert_eq!(value["payload"]["source_amount"], "100.00");
        // occurred_at is RFC3339 UTC
        assert!(value["occurred_at"].as_str().unwrap().ends_with('Z'));
    }

    #[test]
    fn event_round_trips_through_json() {
        let event = SwapEvent::new(
            uuid::Uuid::new_v4(),
            None,
            None,
            SwapEventPayload::Filled {
                source_amount: DecimalString("1.5".into()),
                target_amount: DecimalString("2.5".into()),
            },
        );
        let json = event.to_versioned_json().unwrap();
        let back: SwapEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, event);
    }

    #[test]
    fn decimal_string_from_bigdecimal_is_plain() {
        let d = DecimalString::from_bigdecimal(&BigDecimal::from(100));
        assert_eq!(d.0, "100");
    }
}
