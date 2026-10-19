//! Pre-trade risk and compliance gate for swap orders.
//!
//! [`RiskPolicy::evaluate`] is pure: it takes the order, the account's and
//! tenant's rolling-window usage (computed separately by
//! `db::queries::get_account_rolling_volume` / `get_tenant_rolling_volume`),
//! and returns `Ok(())` or a [`RiskRejection`]. The policy is built once from
//! [`SwapRiskConfig`], and its [`RiskPolicy::version`] is stamped on every
//! decision so a rejection can be replayed against the same configuration.
//!
//! # Check precedence
//!
//! Checks run in this order and the first failure wins:
//!
//! 1. Kill switch (`SWAP_KILL_SWITCH=true`) rejects everything.
//! 2. Deny list. A denied account is rejected even if it is also allowlisted.
//! 3. Allow list. When `SWAP_ACCOUNT_ALLOWLIST` is non-empty, unlisted accounts are rejected.
//! 4. Asset pair must be in `SWAP_ALLOWED_PAIRS`.
//! 5. Order amount must be `<= SWAP_MAX_ORDER_AMOUNT`.
//! 6. Account rolling volume plus this order must be `<= SWAP_MAX_ACCOUNT_VOLUME`.
//! 7. The order must carry a tenant. Tenant limits cannot be applied without one.
//! 8. Tenant rolling volume plus this order must be `<= SWAP_MAX_TENANT_VOLUME`.
//!
//! Limits are inclusive: an amount exactly at the limit passes, one unit over fails.
//!
//! # Concurrency
//!
//! The rolling-volume check reads usage and then compares it. It does not lock
//! or reserve anything, so concurrent orders for the same account or tenant can
//! each see the same usage and together exceed the window cap. This is the
//! accepted residual risk of an advisory check. Making it strict requires a
//! transactional reservation (for example a `SELECT ... FOR UPDATE` on a
//! per-account row) and is out of scope here.

use crate::config::SwapRiskConfig;
use crate::swap::error::{RiskRejection, RiskRejectionCode};
use sha2::{Digest, Sha256};
use sqlx::types::BigDecimal;
use uuid::Uuid;

/// The order under evaluation.
#[derive(Debug, Clone, PartialEq)]
pub struct SwapOrderRisk {
    /// Stellar account placing the order.
    pub account: String,
    /// Owning tenant, when resolved. `None` is rejected once tenant limits apply.
    pub tenant_id: Option<Uuid>,
    /// Asset being sold. Rolling volume is measured in this asset.
    pub source_asset: String,
    /// Asset being bought.
    pub target_asset: String,
    /// Order amount, in `source_asset` units.
    pub amount: BigDecimal,
}

/// Rolling-window volume already committed, in `source_asset` units,
/// excluding the order under evaluation.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RiskUsage {
    pub account_volume: BigDecimal,
    pub tenant_volume: BigDecimal,
}

/// A loaded, versioned risk policy.
#[derive(Debug, Clone)]
pub struct RiskPolicy {
    config: SwapRiskConfig,
    version: String,
}

impl RiskPolicy {
    /// Build the policy. The version is a SHA-256 prefix of the canonical
    /// config, so identical configuration always yields the same identifier.
    pub fn new(config: SwapRiskConfig) -> Self {
        let canonical = format!(
            "kill={}|pairs={:?}|order={}|account={}|tenant={}|window={}|allow={:?}|deny={:?}",
            config.kill_switch,
            config.allowed_pairs,
            config.max_order_amount,
            config.max_account_volume,
            config.max_tenant_volume,
            config.rolling_window_secs,
            config.account_allowlist,
            config.account_denylist,
        );
        let digest = Sha256::digest(canonical.as_bytes());
        let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
        Self {
            config,
            version: format!("swap-risk-{hex}"),
        }
    }

    /// Identifier of the configuration this policy was built from.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Rolling window length, in seconds, that usage must be measured over.
    pub fn rolling_window_secs(&self) -> i64 {
        self.config.rolling_window_secs
    }

    /// Evaluate an order against the policy. See the module docs for precedence.
    pub fn evaluate(&self, order: &SwapOrderRisk, usage: &RiskUsage) -> Result<(), RiskRejection> {
        let c = &self.config;

        if c.kill_switch {
            return self.reject(RiskRejectionCode::KillSwitchActive);
        }
        if c.account_denylist.contains(&order.account) {
            return self.reject(RiskRejectionCode::AccountDenied);
        }
        if !c.account_allowlist.is_empty() && !c.account_allowlist.contains(&order.account) {
            return self.reject(RiskRejectionCode::AccountNotAllowed);
        }

        let pair_allowed = c.allowed_pairs.iter().any(|(source, target)| {
            *source == order.source_asset && *target == order.target_asset
        });
        if !pair_allowed {
            return self.reject(RiskRejectionCode::AssetPairNotAllowed);
        }

        if order.amount > c.max_order_amount {
            return self.reject(RiskRejectionCode::OrderAmountExceeded);
        }
        if &usage.account_volume + &order.amount > c.max_account_volume {
            return self.reject(RiskRejectionCode::AccountVolumeExceeded);
        }

        if order.tenant_id.is_none() {
            return self.reject(RiskRejectionCode::TenantUnresolved);
        }
        if &usage.tenant_volume + &order.amount > c.max_tenant_volume {
            return self.reject(RiskRejectionCode::TenantVolumeExceeded);
        }

        Ok(())
    }

    fn reject(&self, code: RiskRejectionCode) -> Result<(), RiskRejection> {
        Err(RiskRejection {
            code,
            policy_version: self.version.clone(),
        })
    }
}

/// Audit record for a rejected order. Carries the full reason, the policy
/// version, and the order inputs, so a decision can be reviewed and replayed.
/// The caller persists it to the event log.
pub fn rejection_audit_value(
    order: &SwapOrderRisk,
    rejection: &RiskRejection,
) -> serde_json::Value {
    serde_json::json!({
        "decision": "rejected",
        "code": rejection.code.code(),
        "reason": rejection.code.client_message(),
        "policy_version": rejection.policy_version,
        "account": order.account,
        "tenant_id": order.tenant_id,
        "source_asset": order.source_asset,
        "target_asset": order.target_asset,
        "amount": order.amount.to_string(),
    })
}
