//! Error types for the Phase 2 swap engine.
//!
//! Follows the conventions in [`crate::error`]: every variant maps to a stable
//! `ERR_SWAP_*` code and HTTP status in `AppError`, and `SwapError` reaches the
//! crate-level error type through `AppError::Swap`. Because `Internal` carries
//! free-form engine text, it is redacted from client responses like the other
//! wrapping variants.
//!
//! This module also defines typed pre-trade rejections for swap orders.
//! Codes are stable and machine-readable (`ERR_SWAP_RISK_*`); never rename or
//! reuse one. Messages are fixed strings that contain no limit values,
//! account identifiers, or internal detail, so they are safe to return to
//! clients. The precise reason is kept in the audit record, not the response.

use std::fmt;

use thiserror::Error;

/// Error returned by a [`SwapEngine`](super::SwapEngine) or by any swap
/// submodule boundary trait ([`QuoteSource`](super::quote::QuoteSource),
/// [`ExecutionVenue`](super::execution::ExecutionVenue),
/// [`SettlementAdapter`](super::settlement::SettlementAdapter)).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SwapError {
    /// The engine is unreachable or not ready.
    #[error("swap engine unavailable")]
    Unavailable,
    /// The hand-off is malformed or references data the engine cannot use.
    #[error("invalid swap hand-off: {0}")]
    InvalidHandoff(String),
    /// The engine failed internally.
    #[error("swap engine internal error: {0}")]
    Internal(String),
}

/// Why a swap order was blocked by the risk gate. Checks are evaluated in
/// declaration order, so the first failing check is the one reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskRejectionCode {
    /// The global kill switch is on.
    KillSwitchActive,
    /// The account is on the deny list.
    AccountDenied,
    /// An allowlist is configured and the account is not on it.
    AccountNotAllowed,
    /// The source/target asset pair is not in the allowed set.
    AssetPairNotAllowed,
    /// The order amount exceeds the per-order maximum.
    OrderAmountExceeded,
    /// The order would push the account over its rolling-window maximum.
    AccountVolumeExceeded,
    /// The order would push the tenant over its rolling-window maximum.
    TenantVolumeExceeded,
    /// The order has no tenant, so tenant limits cannot be applied.
    TenantUnresolved,
}

impl RiskRejectionCode {
    /// Stable machine-readable code returned to clients and written to the audit log.
    pub fn code(&self) -> &'static str {
        match self {
            RiskRejectionCode::KillSwitchActive => "ERR_SWAP_RISK_001",
            RiskRejectionCode::AccountDenied => "ERR_SWAP_RISK_002",
            RiskRejectionCode::AccountNotAllowed => "ERR_SWAP_RISK_003",
            RiskRejectionCode::AssetPairNotAllowed => "ERR_SWAP_RISK_004",
            RiskRejectionCode::OrderAmountExceeded => "ERR_SWAP_RISK_005",
            RiskRejectionCode::AccountVolumeExceeded => "ERR_SWAP_RISK_006",
            RiskRejectionCode::TenantVolumeExceeded => "ERR_SWAP_RISK_007",
            RiskRejectionCode::TenantUnresolved => "ERR_SWAP_RISK_008",
        }
    }

    /// Human-safe message. Contains no limit values or identifiers.
    pub fn client_message(&self) -> &'static str {
        match self {
            RiskRejectionCode::KillSwitchActive => "Swap activity is temporarily suspended",
            RiskRejectionCode::AccountDenied => "This account is not permitted to swap",
            RiskRejectionCode::AccountNotAllowed => "This account is not permitted to swap",
            RiskRejectionCode::AssetPairNotAllowed => "This asset pair is not supported",
            RiskRejectionCode::OrderAmountExceeded => "Order amount exceeds the permitted limit",
            RiskRejectionCode::AccountVolumeExceeded => {
                "Account swap volume limit reached for the current window"
            }
            RiskRejectionCode::TenantVolumeExceeded => {
                "Tenant swap volume limit reached for the current window"
            }
            RiskRejectionCode::TenantUnresolved => "Swap requires a resolvable tenant",
        }
    }
}

/// A rejected swap order. Carries the policy version that made the decision
/// so the rejection can be replayed against the same configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskRejection {
    pub code: RiskRejectionCode,
    pub policy_version: String,
}

impl fmt::Display for RiskRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.code(), self.code.client_message())
    }
}

impl std::error::Error for RiskRejection {}

