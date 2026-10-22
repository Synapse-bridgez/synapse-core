//! Asset / pair lifecycle status and administrative actions.
//!
//! Status is an explicit state consulted by validation and quoting (via
//! [`AssetStatus::allows_quotes`] / [`allows_new_orders`]) rather than ad-hoc
//! `if delisted` checks. Actions are idempotent: re-applying an action whose
//! target status is already current is a no-op. Persistence lives in
//! `asset_status` / `asset_admin_actions` (append-only audit); delisting never
//! deletes historical rows needed for reconciliation or reporting. Replicas
//! read status from the database, so changes apply without restart.

use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetStatus {
    Active,
    Suspended,
    Frozen,
    Delisted,
}

impl AssetStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            AssetStatus::Active => "ACTIVE",
            AssetStatus::Suspended => "SUSPENDED",
            AssetStatus::Frozen => "FROZEN",
            AssetStatus::Delisted => "DELISTED",
        }
    }

    /// Quoting stops immediately for anything but ACTIVE.
    pub fn allows_quotes(&self) -> bool {
        *self == AssetStatus::Active
    }

    pub fn allows_new_orders(&self) -> bool {
        *self == AssetStatus::Active
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminAction {
    SuspendPair,
    ResumePair,
    FreezeAsset,
    DelistAsset,
}

impl AdminAction {
    pub fn target(&self) -> AssetStatus {
        match self {
            AdminAction::SuspendPair => AssetStatus::Suspended,
            AdminAction::ResumePair => AssetStatus::Active,
            AdminAction::FreezeAsset => AssetStatus::Frozen,
            AdminAction::DelistAsset => AssetStatus::Delisted,
        }
    }

    /// Delisting is irreversible and needs elevated authorization.
    pub fn requires_elevated_auth(&self) -> bool {
        *self == AdminAction::DelistAsset
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransitionError {
    /// DELISTED is terminal.
    AlreadyDelisted,
    /// A frozen asset can only be resumed or delisted by an elevated actor.
    NotPermitted {
        from: AssetStatus,
        action: AdminAction,
    },
    ElevatedAuthRequired,
    MissingReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transition {
    /// Status unchanged (idempotent re-apply).
    NoOp,
    Changed {
        from: AssetStatus,
        to: AssetStatus,
    },
}

/// Validate and compute a status change.
pub fn apply(
    current: AssetStatus,
    action: AdminAction,
    reason: &str,
    elevated: bool,
) -> Result<Transition, TransitionError> {
    if reason.trim().is_empty() {
        return Err(TransitionError::MissingReason);
    }
    if action.requires_elevated_auth() && !elevated {
        return Err(TransitionError::ElevatedAuthRequired);
    }
    let to = action.target();
    if current == AssetStatus::Delisted {
        return if to == AssetStatus::Delisted {
            Ok(Transition::NoOp)
        } else {
            Err(TransitionError::AlreadyDelisted)
        };
    }
    if current == to {
        return Ok(Transition::NoOp);
    }
    // Resuming a frozen asset requires an elevated actor.
    if current == AssetStatus::Frozen && action == AdminAction::ResumePair && !elevated {
        return Err(TransitionError::NotPermitted {
            from: current,
            action,
        });
    }
    Ok(Transition::Changed { from: current, to })
}

/// Policy for work already in flight when status leaves ACTIVE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InFlightPolicy {
    /// Cancel open (untriggered) orders and unfilled quotes; allow executing
    /// orders and settlement of filled orders to complete.
    DrainAndCancelOpen,
    /// Cancel everything not yet filled; filled orders are compensated.
    CancelAll,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderPhase {
    OpenQuote,
    OpenOrder,
    Executing,
    FilledUnsettled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InFlightEffect {
    /// Cancel and notify the tenant.
    Cancel,
    /// Let it complete.
    Complete,
    /// Reverse the fill / refund; never half-settle silently.
    Compensate,
}

pub fn in_flight_effect(
    new_status: AssetStatus,
    policy: InFlightPolicy,
    phase: OrderPhase,
) -> InFlightEffect {
    use InFlightEffect::*;
    use OrderPhase::*;
    if new_status == AssetStatus::Active {
        return Complete;
    }
    match (policy, phase) {
        (_, OpenQuote) | (_, OpenOrder) => Cancel,
        (InFlightPolicy::DrainAndCancelOpen, Executing | FilledUnsettled) => Complete,
        // Delisting/freeze with CancelAll: executing orders are cancelled
        // (compensated if partially applied), filled-unsettled are compensated.
        (InFlightPolicy::CancelAll, Executing) => Compensate,
        (InFlightPolicy::CancelAll, FilledUnsettled) => Compensate,
    }
}

/// Audit record for an administrative action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminActionRecord {
    pub action: AdminAction,
    pub target: String,
    pub actor: String,
    pub reason: String,
    pub effective_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use AdminAction::*;
    use AssetStatus::*;

    #[test]
    fn permitted_transitions() {
        for (from, act, to) in [
            (Active, SuspendPair, Suspended),
            (Suspended, ResumePair, Active),
            (Active, FreezeAsset, Frozen),
            (Suspended, FreezeAsset, Frozen),
            (Active, DelistAsset, Delisted),
            (Frozen, DelistAsset, Delisted),
        ] {
            assert_eq!(
                apply(from, act, "r", true),
                Ok(Transition::Changed { from, to })
            );
        }
    }

    #[test]
    fn forbidden_transitions() {
        for act in [SuspendPair, ResumePair, FreezeAsset] {
            assert_eq!(
                apply(Delisted, act, "r", true),
                Err(TransitionError::AlreadyDelisted)
            );
        }
        assert!(apply(Frozen, ResumePair, "r", false).is_err());
        assert_eq!(
            apply(Active, DelistAsset, "r", false),
            Err(TransitionError::ElevatedAuthRequired)
        );
        assert_eq!(
            apply(Active, SuspendPair, " ", true),
            Err(TransitionError::MissingReason)
        );
    }

    #[test]
    fn idempotent() {
        assert_eq!(
            apply(Suspended, SuspendPair, "r", false),
            Ok(Transition::NoOp)
        );
        assert_eq!(
            apply(Delisted, DelistAsset, "r", true),
            Ok(Transition::NoOp)
        );
    }

    #[test]
    fn non_active_blocks_quotes_and_orders() {
        assert!(Active.allows_quotes() && Active.allows_new_orders());
        for s in [Suspended, Frozen, Delisted] {
            assert!(!s.allows_quotes() && !s.allows_new_orders());
        }
    }

    #[test]
    fn in_flight_semantics() {
        use InFlightEffect::*;
        use InFlightPolicy::*;
        use OrderPhase::*;
        assert_eq!(
            in_flight_effect(Suspended, DrainAndCancelOpen, OpenOrder),
            Cancel
        );
        assert_eq!(
            in_flight_effect(Suspended, DrainAndCancelOpen, Executing),
            Complete
        );
        assert_eq!(
            in_flight_effect(Delisted, CancelAll, FilledUnsettled),
            Compensate
        );
        assert_eq!(in_flight_effect(Delisted, CancelAll, OpenQuote), Cancel);
    }
}
