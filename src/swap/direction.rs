//! Swap direction as data: `fiat_to_stellar` (on-ramp) and `stellar_to_fiat`
//! (off-ramp). Quoting, execution and risk are shared; only the anchor
//! interaction and settlement confirmation branch on direction.
//! See `docs/swap-directions.md`.

use serde::{Deserialize, Serialize};
use sqlx::types::BigDecimal;
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SwapDirection {
    #[default]
    FiatToStellar,
    StellarToFiat,
}

impl SwapDirection {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::FiatToStellar => "fiat_to_stellar",
            Self::StellarToFiat => "stellar_to_fiat",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "fiat_to_stellar" => Some(Self::FiatToStellar),
            "stellar_to_fiat" => Some(Self::StellarToFiat),
            _ => None,
        }
    }
}

/// Off-ramp order lifecycle. `Debited` means the Stellar asset has been taken
/// from the user; any failure after that point must compensate (refund).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OffRampStatus {
    Quoted,
    Debited,
    AnchorPending,
    Completed,
    Compensating,
    Refunded,
    Cancelled,
}

impl OffRampStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Refunded | Self::Cancelled)
    }
}

/// Anchor callback outcomes (the anchor retries, so these are idempotent).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AnchorEvent {
    Pending,
    Completed,
    Rejected { reason: String },
    TimedOut,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallbackDecision {
    /// Apply this transition.
    Transition(OffRampStatus),
    /// Same callback seen before (or order already terminal): acknowledge, no-op.
    Replay,
    /// Event is not valid in the current state.
    Invalid(String),
}

/// Decide the effect of an anchor callback. `callback_id` is the anchor's
/// event id; `seen` is the set of ids already applied to this order.
pub fn decide_callback(
    current: OffRampStatus,
    callback_id: &str,
    seen: &HashSet<String>,
    event: &AnchorEvent,
) -> CallbackDecision {
    use OffRampStatus::*;
    if seen.contains(callback_id) || current.is_terminal() {
        return CallbackDecision::Replay;
    }
    match (current, event) {
        (Debited | AnchorPending, AnchorEvent::Pending) => {
            CallbackDecision::Transition(AnchorPending)
        }
        (Debited | AnchorPending, AnchorEvent::Completed) => {
            CallbackDecision::Transition(Completed)
        }
        // Rejection/timeout after the asset was debited triggers compensation.
        (Debited | AnchorPending, AnchorEvent::Rejected { .. } | AnchorEvent::TimedOut) => {
            CallbackDecision::Transition(Compensating)
        }
        (Compensating, _) => CallbackDecision::Replay,
        (s, e) => CallbackDecision::Invalid(format!("{e:?} not valid in {s:?}")),
    }
}

/// Mid-flight cancellation: allowed only before the asset is debited;
/// afterwards the order must go through compensation instead.
pub fn decide_cancel(current: OffRampStatus) -> CallbackDecision {
    match current {
        OffRampStatus::Quoted => CallbackDecision::Transition(OffRampStatus::Cancelled),
        OffRampStatus::Debited | OffRampStatus::AnchorPending => {
            CallbackDecision::Transition(OffRampStatus::Compensating)
        }
        s if s.is_terminal() => CallbackDecision::Replay,
        s => CallbackDecision::Invalid(format!("cannot cancel in {s:?}")),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OffRampError {
    #[error("unsupported asset/currency pair {0}/{1}")]
    UnsupportedPair(String, String),
    #[error("amount below anchor minimum")]
    BelowAnchorMinimum,
}

/// Anchor off-ramp configuration (see `src/config.rs` for sourcing).
#[derive(Debug, Clone, Default)]
pub struct AnchorOffRampConfig {
    pub supported_pairs: HashSet<(String, String)>,
    pub min_amount: BigDecimal,
}

pub fn validate_offramp(
    cfg: &AnchorOffRampConfig,
    asset: &str,
    currency: &str,
    amount: &BigDecimal,
) -> Result<(), OffRampError> {
    if !cfg
        .supported_pairs
        .contains(&(asset.to_string(), currency.to_string()))
    {
        return Err(OffRampError::UnsupportedPair(asset.into(), currency.into()));
    }
    if amount < &cfg.min_amount {
        return Err(OffRampError::BelowAnchorMinimum);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn default_direction_is_existing_flow() {
        assert_eq!(SwapDirection::default().as_str(), "fiat_to_stellar");
        assert_eq!(
            SwapDirection::parse("stellar_to_fiat"),
            Some(SwapDirection::StellarToFiat)
        );
    }

    #[test]
    fn callbacks_are_idempotent_and_compensate_on_rejection() {
        let mut seen = HashSet::new();
        let rej = AnchorEvent::Rejected {
            reason: "kyc".into(),
        };
        assert_eq!(
            decide_callback(OffRampStatus::AnchorPending, "e1", &seen, &rej),
            CallbackDecision::Transition(OffRampStatus::Compensating)
        );
        seen.insert("e1".to_string());
        assert_eq!(
            decide_callback(OffRampStatus::AnchorPending, "e1", &seen, &rej),
            CallbackDecision::Replay
        );
        assert_eq!(
            decide_callback(OffRampStatus::Completed, "e2", &seen, &AnchorEvent::Pending),
            CallbackDecision::Replay
        );
    }

    #[test]
    fn cancel_after_debit_compensates() {
        assert_eq!(
            decide_cancel(OffRampStatus::Quoted),
            CallbackDecision::Transition(OffRampStatus::Cancelled)
        );
        assert_eq!(
            decide_cancel(OffRampStatus::Debited),
            CallbackDecision::Transition(OffRampStatus::Compensating)
        );
    }

    #[test]
    fn rejects_unsupported_pair_and_small_amounts() {
        let cfg = AnchorOffRampConfig {
            supported_pairs: HashSet::from([("USDC".to_string(), "NGN".to_string())]),
            min_amount: BigDecimal::from_str("10").unwrap(),
        };
        let ok = BigDecimal::from_str("50").unwrap();
        assert!(validate_offramp(&cfg, "USDC", "NGN", &ok).is_ok());
        assert!(validate_offramp(&cfg, "USDC", "EUR", &ok).is_err());
        assert_eq!(
            validate_offramp(&cfg, "USDC", "NGN", &BigDecimal::from(1)),
            Err(OffRampError::BelowAnchorMinimum)
        );
    }
}
