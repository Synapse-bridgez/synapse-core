//! Cross-chain bridge handoff interface for settled swaps.
//!
//! This module defines the boundary between the swap engine (Phase 1/2) and the
//! cross-chain bridging phase described in the README roadmap. A fully settled
//! swap is handed off to a [`BridgeHandoff`] implementation exactly once, via a
//! well-formed, versioned [`BridgeHandoffPayload`]. The handoff's lifecycle is
//! tracked through [`HandoffStatus`] and can be retried/replayed when it fails.
//!
//! The trait is the only coupling point between the swap engine and any bridge
//! adapter, so a real bridge integration can be swapped in later without
//! touching settlement logic.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// Current version of the handoff payload schema.
///
/// Bumped whenever the payload shape changes in a backwards-incompatible way,
/// mirroring the versioning style used by other swap lifecycle events.
pub const BRIDGE_HANDOFF_PAYLOAD_VERSION: u32 = 1;

/// Canonical, versioned payload describing a settled swap being handed off to
/// the cross-chain bridging phase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeHandoffPayload {
    /// Schema version of this payload.
    pub version: u32,
    /// Source asset identifier (e.g. `"BTC"`, `"USDC"`).
    pub source_asset: String,
    /// Source amount in the asset's smallest unit.
    pub source_amount: u64,
    /// Destination chain identifier (e.g. `"ethereum"`, `"solana"`).
    pub destination_chain: String,
    /// Destination address on the target chain.
    pub destination_address: String,
    /// Correlation key linking this handoff back to the originating swap.
    pub correlation_key: String,
    /// Idempotency key ensuring the handoff is produced exactly once.
    pub idempotency_key: String,
    /// Reference to the settlement that produced this handoff.
    pub settlement_reference: String,
}

impl BridgeHandoffPayload {
    /// Constructs a new handoff payload at the current schema version.
    ///
    /// Returns an error if any required field is empty, which guards against
    /// emitting handoffs with missing destination fields.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source_asset: impl Into<String>,
        source_amount: u64,
        destination_chain: impl Into<String>,
        destination_address: impl Into<String>,
        correlation_key: impl Into<String>,
        idempotency_key: impl Into<String>,
        settlement_reference: impl Into<String>,
    ) -> Result<Self, HandoffError> {
        let payload = Self {
            version: BRIDGE_HANDOFF_PAYLOAD_VERSION,
            source_asset: source_asset.into(),
            source_amount,
            destination_chain: destination_chain.into(),
            destination_address: destination_address.into(),
            correlation_key: correlation_key.into(),
            idempotency_key: idempotency_key.into(),
            settlement_reference: settlement_reference.into(),
        };
        payload.validate()?;
        Ok(payload)
    }

    /// Validates that all required fields are present and well-formed.
    pub fn validate(&self) -> Result<(), HandoffError> {
        if self.version != BRIDGE_HANDOFF_PAYLOAD_VERSION {
            return Err(HandoffError::UnsupportedVersion(self.version));
        }
        if self.source_asset.trim().is_empty() {
            return Err(HandoffError::MissingField("source_asset"));
        }
        if self.source_amount == 0 {
            return Err(HandoffError::MissingField("source_amount"));
        }
        if self.destination_chain.trim().is_empty() {
            return Err(HandoffError::MissingField("destination_chain"));
        }
        if self.destination_address.trim().is_empty() {
            return Err(HandoffError::MissingField("destination_address"));
        }
        if self.correlation_key.trim().is_empty() {
            return Err(HandoffError::MissingField("correlation_key"));
        }
        if self.idempotency_key.trim().is_empty() {
            return Err(HandoffError::MissingField("idempotency_key"));
        }
        if self.settlement_reference.trim().is_empty() {
            return Err(HandoffError::MissingField("settlement_reference"));
        }
        Ok(())
    }
}

/// Lifecycle status of a bridge handoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HandoffStatus {
    /// Handoff has been produced and is awaiting acknowledgement.
    Pending,
    /// Handoff has been acknowledged by the bridge adapter.
    Acknowledged,
    /// Handoff failed and may be retried/replayed.
    Failed,
}

/// Errors that can occur while producing or tracking a handoff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandoffError {
    /// A required payload field was missing or empty.
    MissingField(&'static str),
    /// The payload used an unsupported schema version.
    UnsupportedVersion(u32),
    /// The swap was not in a settled state and must not be handed off.
    NotSettled,
    /// A handoff with the same idempotency key already exists.
    Duplicate(String),
    /// The handoff could not be found.
    NotFound(String),
    /// The handoff is not in a state that permits the requested operation.
    InvalidState {
        /// The current status of the handoff.
        current: HandoffStatus,
    },
}

impl std::fmt::Display for HandoffError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HandoffError::MissingField(field) => {
                write!(f, "missing required handoff field: {field}")
            }
            HandoffError::UnsupportedVersion(v) => {
                write!(f, "unsupported handoff payload version: {v}")
            }
            HandoffError::NotSettled => write!(f, "swap is not settled; handoff not permitted"),
            HandoffError::Duplicate(key) => {
                write!(f, "handoff already exists for idempotency key: {key}")
            }
            HandoffError::NotFound(key) => write!(f, "handoff not found: {key}"),
            HandoffError::InvalidState { current } => {
                write!(f, "handoff is in invalid state: {current:?}")
            }
        }
    }
}

impl std::error::Error for HandoffError {}

/// Result of producing a handoff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandoffOutcome {
    /// A new handoff was produced.
    Produced(BridgeHandoffPayload),
    /// A handoff already existed for this idempotency key; nothing was emitted.
    AlreadyExists(BridgeHandoffPayload),
}

/// The bridge handoff boundary.
///
/// Implementations receive a settled swap's payload exactly once and are
/// responsible for forwarding it to the bridging phase. The trait is the only
/// coupling point between the swap engine and any bridge adapter.
pub trait BridgeHandoff: Send + Sync {
    /// Produces a handoff for a settled swap.
    ///
    /// Implementations MUST be idempotent with respect to
    /// [`BridgeHandoffPayload::idempotency_key`]: repeated calls with the same
    /// key must not produce a second handoff.
    fn handoff(&self, payload: BridgeHandoffPayload) -> Result<HandoffOutcome, HandoffError>;

    /// Returns the current status of the handoff identified by `idempotency_key`.
    fn status(&self, idempotency_key: &str) -> Result<HandoffStatus, HandoffError>;

    /// Acknowledges a pending handoff.
    fn acknowledge(&self, idempotency_key: &str) -> Result<HandoffStatus, HandoffError>;

    /// Marks a pending handoff as failed.
    fn fail(&self, idempotency_key: &str) -> Result<HandoffStatus, HandoffError>;

    /// Retries/replays a failed handoff, returning it to `Pending`.
    fn retry(&self, idempotency_key: &str) -> Result<HandoffStatus, HandoffError>;
}

/// A local, no-op [`BridgeHandoff`] implementation.
///
/// Records handoffs in memory and tracks their status. Useful for wiring the
/// boundary before a real bridge adapter exists, and for tests.
#[derive(Debug, Default)]
pub struct NoopBridgeHandoff {
    inner: Mutex<HashMap<String, (BridgeHandoffPayload, HandoffStatus)>>,
}

impl NoopBridgeHandoff {
    /// Creates a new, empty no-op handoff implementation.
    pub fn new() -> Self {
        Self::default()
    }
}

impl BridgeHandoff for NoopBridgeHandoff {
    fn handoff(&self, payload: BridgeHandoffPayload) -> Result<HandoffOutcome, HandoffError> {
        payload.validate()?;
        let key = payload.idempotency_key.clone();
        let mut guard = self.inner.lock().expect("handoff mutex poisoned");
        if let Some((existing, _)) = guard.get(&key) {
            return Ok(HandoffOutcome::AlreadyExists(existing.clone()));
        }
        guard.insert(key, (payload.clone(), HandoffStatus::Pending));
        Ok(HandoffOutcome::Produced(payload))
    }

    fn status(&self, idempotency_key: &str) -> Result<HandoffStatus, HandoffError> {
        let guard = self.inner.lock().expect("handoff mutex poisoned");
        guard
            .get(idempotency_key)
            .map(|(_, status)| *status)
            .ok_or_else(|| HandoffError::NotFound(idempotency_key.to_string()))
    }

    fn acknowledge(&self, idempotency_key: &str) -> Result<HandoffStatus, HandoffError> {
        self.transition(idempotency_key, HandoffStatus::Acknowledged)
    }

    fn fail(&self, idempotency_key: &str) -> Result<HandoffStatus, HandoffError> {
        self.transition(idempotency_key, HandoffStatus::Failed)
    }

    fn retry(&self, idempotency_key: &str) -> Result<HandoffStatus, HandoffError> {
        let mut guard = self.inner.lock().expect("handoff mutex poisoned");
        let entry = guard
            .get_mut(idempotency_key)
            .ok_or_else(|| HandoffError::NotFound(idempotency_key.to_string()))?;
        if entry.1 != HandoffStatus::Failed {
            return Err(HandoffError::InvalidState { current: entry.1 });
        }
        entry.1 = HandoffStatus::Pending;
        Ok(HandoffStatus::Pending)
    }
}

impl NoopBridgeHandoff {
    fn transition(
        &self,
        idempotency_key: &str,
        next: HandoffStatus,
    ) -> Result<HandoffStatus, HandoffError> {
        let mut guard = self.inner.lock().expect("handoff mutex poisoned");
        let entry = guard
            .get_mut(idempotency_key)
            .ok_or_else(|| HandoffError::NotFound(idempotency_key.to_string()))?;
        if entry.1 != HandoffStatus::Pending {
            return Err(HandoffError::InvalidState { current: entry.1 });
        }
        entry.1 = next;
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_payload() -> BridgeHandoffPayload {
        BridgeHandoffPayload::new(
            "BTC",
            100_000,
            "ethereum",
            "0xabc123",
            "swap-42",
            "idem-42",
            "settlement-42",
        )
        .expect("valid payload")
    }

    #[test]
    fn payload_construction_sets_version_and_fields() {
        let payload = sample_payload();
        assert_eq!(payload.version, BRIDGE_HANDOFF_PAYLOAD_VERSION);
        assert_eq!(payload.source_asset, "BTC");
        assert_eq!(payload.source_amount, 100_000);
        assert_eq!(payload.destination_chain, "ethereum");
        assert_eq!(payload.destination_address, "0xabc123");
        assert_eq!(payload.correlation_key, "swap-42");
        assert_eq!(payload.idempotency_key, "idem-42");
        assert_eq!(payload.settlement_reference, "settlement-42");
    }

    #[test]
    fn payload_construction_rejects_missing_destination() {
        let err = BridgeHandoffPayload::new(
            "BTC",
            100_000,
            "ethereum",
            "",
            "swap-42",
            "idem-42",
            "settlement-42",
        )
        .unwrap_err();
        assert_eq!(err, HandoffError::MissingField("destination_address"));
    }

    #[test]
    fn payload_construction_rejects_zero_amount() {
        let err = BridgeHandoffPayload::new(
            "BTC",
            0,
            "ethereum",
            "0xabc123",
            "swap-42",
            "idem-42",
            "settlement-42",
        )
        .unwrap_err();
        assert_eq!(err, HandoffError::MissingField("source_amount"));
    }

    #[test]
    fn payload_roundtrips_through_serde() {
        let payload = sample_payload();
        let json = serde_json::to_string(&payload).unwrap();
        let decoded: BridgeHandoffPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(payload, decoded);
    }

    #[test]
    fn handoff_is_produced_exactly_once() {
        let handoff = NoopBridgeHandoff::new();
        let payload = sample_payload();

        let first = handoff.handoff(payload.clone()).unwrap();
        assert_eq!(first, HandoffOutcome::Produced(payload.clone()));

        let second = handoff.handoff(payload.clone()).unwrap();
        assert_eq!(second, HandoffOutcome::AlreadyExists(payload.clone()));

        assert_eq!(
            handoff.status("idem-42").unwrap(),
            HandoffStatus::Pending
        );
    }

    #[test]
    fn status_transitions_and_retry() {
        let handoff = NoopBridgeHandoff::new();
        handoff.handoff(sample_payload()).unwrap();

        assert_eq!(
            handoff.fail("idem-42").unwrap(),
            HandoffStatus::Failed
        );
        assert_eq!(
            handoff.retry("idem-42").unwrap(),
            HandoffStatus::Pending
        );
        assert_eq!(
            handoff.acknowledge("idem-42").unwrap(),
            HandoffStatus::Acknowledged
        );

        // Acknowledged handoffs cannot be retried.
        assert_eq!(
            handoff.retry("idem-42").unwrap_err(),
            HandoffError::InvalidState {
                current: HandoffStatus::Acknowledged
            }
        );
    }

    #[test]
    fn retry_requires_failed_state() {
        let handoff = NoopBridgeHandoff::new();
        handoff.handoff(sample_payload()).unwrap();
        assert_eq!(
            handoff.retry("idem-42").unwrap_err(),
            HandoffError::InvalidState {
                current: HandoffStatus::Pending
            }
        );
    }

    #[test]
    fn unknown_handoff_returns_not_found() {
        let handoff = NoopBridgeHandoff::new();
        assert_eq!(
            handoff.status("missing").unwrap_err(),
            HandoffError::NotFound("missing".to_string())
        );
    }
}