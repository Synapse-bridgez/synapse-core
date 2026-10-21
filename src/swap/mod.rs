//! Phase 2 swap-engine domain.
//!
//! The Phase 1 swap-engine hand-off stub (trait, no-op impl, hand-off type) has been
//! retired (#1531). Phase 1 `swap_ready` rows remain valid persisted
//! transaction states and are left as fiat-only transactions; no code path
//! hands them to a no-op engine any more. Swap behaviour is gated by the
//! `swap_engine` feature flag, which defaults to off.

pub mod capacity;
pub mod liquidity;
pub mod settlement;

use crate::services::feature_flags::FeatureFlagService;

/// Feature flag that opts a tenant into Phase 2 swap handling.
pub const SWAP_ENGINE_FLAG: &str = "swap_engine";

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

#[cfg(test)]
mod tests {
    use crate::validation::state_transitions::{is_valid_transition, TRANSACTION_TRANSITIONS};

    #[test]
    fn phase1_stub_symbols_are_gone() {
        let src = include_str!("mod.rs");
        let tests_at = src.find("#[cfg(test)]").unwrap();
        let code = &src[..tests_at];
        for sym in ["trait SwapEngine", "NoopSwapEngine", "struct SwapHandoff"] {
            assert!(!code.contains(sym), "{sym} must not exist");
        }
    }

    #[test]
    fn legacy_swap_ready_rows_stay_readable() {
        assert!(is_valid_transition(
            "completed",
            "swap_ready",
            TRANSACTION_TRANSITIONS
        ));
        assert!(!is_valid_transition(
            "swap_ready",
            "completed",
            TRANSACTION_TRANSITIONS
        ));
    }
}
