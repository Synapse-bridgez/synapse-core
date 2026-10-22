//! Compensating-transaction planning for failed or partially filled swaps.
//!
//! Each forward swap step has a paired inverse. When a swap fails after funds
//! were committed, or only part of the order filled, [`plan_refund`] works out
//! what returns to the user and [`retry_delay`] decides how long to wait before
//! retrying a transient refund failure.
//!
//! Planning is pure. The caller persists the [`CompensationPlan`] before
//! executing it and writes each [`LedgerEntry`] with its `idempotency_key`, so a
//! replayed compensation cannot refund the same order twice.

use std::time::Duration;

use sqlx::types::BigDecimal;
use uuid::Uuid;

use crate::error::AppError;

/// Stellar amounts carry at most 7 decimal places.
const AMOUNT_SCALE: i64 = 7;

/// How the unswapped remainder of a partially filled order is settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PartialFillPolicy {
    /// Return the unswapped principal and its fee share to the user.
    #[default]
    RefundRemainder,
    /// Leave the unswapped slice on the order for a later fill.
    KeepRemainder,
}

/// Tunables for compensation execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompensationConfig {
    /// Settlement rule for the unswapped remainder.
    pub partial_fill_policy: PartialFillPolicy,
    /// Failed attempts allowed before escalating to manual intervention.
    pub max_attempts: u32,
    /// Delay after the first failed attempt; doubles on each later failure.
    pub base_backoff: Duration,
    /// Upper bound on any single retry delay.
    pub max_backoff: Duration,
}

impl Default for CompensationConfig {
    fn default() -> Self {
        Self {
            partial_fill_policy: PartialFillPolicy::default(),
            max_attempts: 5,
            base_backoff: Duration::from_secs(2),
            max_backoff: Duration::from_secs(300),
        }
    }
}

/// A compensating step. Each one is the inverse of a forward swap step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompensationStep {
    RefundPrincipal,
    RefundFee,
}

impl CompensationStep {
    fn as_str(&self) -> &'static str {
        match self {
            CompensationStep::RefundPrincipal => "refund_principal",
            CompensationStep::RefundFee => "refund_fee",
        }
    }
}

/// A refund credit tied to the original order.
#[derive(Debug, Clone, PartialEq)]
pub struct LedgerEntry {
    pub order_id: Uuid,
    pub step: CompensationStep,
    pub asset_code: String,
    pub amount: BigDecimal,
    /// Dedupe key per order and step: `comp:{order_id}:{step}`.
    pub idempotency_key: String,
}

/// The compensation for one order. Recorded even when nothing is refunded, so
/// every order keeps either a fill or a refund (or an explicit remainder).
#[derive(Debug, Clone, PartialEq)]
pub struct CompensationPlan {
    pub order_id: Uuid,
    pub filled: BigDecimal,
    pub unswapped: BigDecimal,
    pub entries: Vec<LedgerEntry>,
}

/// Build the compensation plan for an order that filled `filled` of
/// `requested`, where `fee` was charged on the full `requested` amount.
///
/// The fee is refunded in proportion to the unswapped share. Under
/// [`PartialFillPolicy::KeepRemainder`] the unswapped slice, fee included,
/// stays on the order and no refund entries are produced.
pub fn plan_refund(
    order_id: Uuid,
    asset_code: &str,
    requested: &BigDecimal,
    filled: &BigDecimal,
    fee: &BigDecimal,
    config: &CompensationConfig,
) -> Result<CompensationPlan, AppError> {
    let zero = BigDecimal::from(0);
    if *requested <= zero {
        return Err(AppError::Validation(
            "requested amount must be positive".to_string(),
        ));
    }
    if *filled < zero || filled > requested {
        return Err(AppError::Validation(
            "filled amount must be between zero and requested amount".to_string(),
        ));
    }
    if *fee < zero {
        return Err(AppError::Validation("fee must not be negative".to_string()));
    }

    let unswapped = requested - filled;
    let mut entries = Vec::new();

    if config.partial_fill_policy == PartialFillPolicy::RefundRemainder && unswapped > zero {
        let fee_refund = (fee * &unswapped / requested).with_scale(AMOUNT_SCALE);

        entries.push(ledger_entry(
            order_id,
            CompensationStep::RefundPrincipal,
            asset_code,
            unswapped.clone(),
        ));
        if fee_refund > zero {
            entries.push(ledger_entry(
                order_id,
                CompensationStep::RefundFee,
                asset_code,
                fee_refund,
            ));
        }
    }

    Ok(CompensationPlan {
        order_id,
        filled: filled.clone(),
        unswapped,
        entries,
    })
}

fn ledger_entry(
    order_id: Uuid,
    step: CompensationStep,
    asset_code: &str,
    amount: BigDecimal,
) -> LedgerEntry {
    LedgerEntry {
        order_id,
        step,
        asset_code: asset_code.to_string(),
        amount,
        idempotency_key: format!("comp:{order_id}:{}", step.as_str()),
    }
}

/// Reject a refund aimed at a target that has since been disabled. The caller
/// should escalate rather than retry, because retrying will not re-enable it.
pub fn ensure_refund_target_enabled(enabled: bool, target: &str) -> Result<(), AppError> {
    if enabled {
        Ok(())
    } else {
        Err(AppError::CompensationTargetDisabled(target.to_string()))
    }
}

/// Delay before retrying a failed refund, using bounded exponential backoff.
///
/// `failed_attempts` counts the failures so far, starting at 1. Once it reaches
/// `max_attempts` this returns [`AppError::CompensationExhausted`], which means
/// the order must go to manual intervention.
pub fn retry_delay(
    config: &CompensationConfig,
    failed_attempts: u32,
) -> Result<Duration, AppError> {
    let failed_attempts = failed_attempts.max(1);
    if failed_attempts >= config.max_attempts {
        return Err(AppError::CompensationExhausted(format!(
            "refund failed {failed_attempts} times"
        )));
    }

    let factor = 1u32.checked_shl(failed_attempts - 1).unwrap_or(u32::MAX);
    Ok(config
        .base_backoff
        .saturating_mul(factor)
        .min(config.max_backoff))
}
