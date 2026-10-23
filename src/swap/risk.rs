//! Pre-execution risk and compliance gate for swap orders.
//!
//! [`RiskGatedSwapEngine`] wraps a [`SwapEngine`] and is the intended way to
//! reach one: every [`SwapEngine::on_swap_ready`] call is checked against
//! [`RiskLimits`] before the wrapped engine is invoked. Breaching orders are
//! recorded as `held` and reported as [`SwapOutcome::Held`] with a structured
//! [`RiskBreach`]. An operator clears one with
//! [`queries::approve_swap_risk_decision`], after which re-delivering the
//! hand-off routes it.
//!
//! The check and its decision are written in one Postgres transaction that
//! first takes a per-tenant advisory lock, so concurrent orders for the same
//! tenant are evaluated one after the other against each other's decisions.
//! Failures to evaluate fail closed: the order is not routed.

use super::{SwapEngine, SwapError, SwapHandoff, SwapOutcome};
use crate::alerting::{names, AlertPayload, AlertSeverity};
use crate::config::RiskLimits;
use crate::db::queries::{self, SwapRiskExposure};
use async_trait::async_trait;
use chrono::{Duration, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::types::BigDecimal;
use sqlx::PgPool;
use std::fmt;
use uuid::Uuid;

/// Why an order was held. Stored as JSON on the decision row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RiskBreach {
    NonPositiveAmount { amount: String },
    OrderSizeExceeded { limit: String, amount: String },
    DailyExposureExceeded { limit: String, projected: String },
    VelocityExceeded { max_orders: u32, window_secs: u32 },
    AssetNotAllowed { asset: String },
    AssetDenied { asset: String },
}

impl fmt::Display for RiskBreach {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RiskBreach::NonPositiveAmount { amount } => {
                write!(f, "order amount {amount} is not positive")
            }
            RiskBreach::OrderSizeExceeded { limit, amount } => {
                write!(f, "order size {amount} exceeds limit {limit}")
            }
            RiskBreach::DailyExposureExceeded { limit, projected } => {
                write!(f, "daily exposure {projected} would exceed limit {limit}")
            }
            RiskBreach::VelocityExceeded {
                max_orders,
                window_secs,
            } => write!(f, "more than {max_orders} orders in {window_secs}s"),
            RiskBreach::AssetNotAllowed { asset } => {
                write!(f, "asset {asset} is not on the allow list")
            }
            RiskBreach::AssetDenied { asset } => write!(f, "asset {asset} is on the deny list"),
        }
    }
}

/// Checks one order against `limits` and returns the first limit it breaches.
/// Pure and deterministic: all tenant state comes in through `exposure`.
///
/// Limits are inclusive. An order equal to `max_order_size`, or a projected
/// daily total equal to `tenant_daily_exposure_limit`, passes. The velocity
/// limit allows `velocity_max_orders` accepted orders per window, so the next
/// one after that is held.
pub fn evaluate(
    limits: &RiskLimits,
    asset: &str,
    amount: &BigDecimal,
    exposure: &SwapRiskExposure,
) -> Result<(), RiskBreach> {
    let asset = asset.to_ascii_uppercase();

    // A zero or negative amount would lower the daily total and bypass the caps.
    if amount <= &BigDecimal::from(0) {
        return Err(RiskBreach::NonPositiveAmount {
            amount: amount.to_string(),
        });
    }
    if amount > &limits.max_order_size {
        return Err(RiskBreach::OrderSizeExceeded {
            limit: limits.max_order_size.to_string(),
            amount: amount.to_string(),
        });
    }
    let projected = &exposure.daily_amount + amount;
    if projected > limits.tenant_daily_exposure_limit {
        return Err(RiskBreach::DailyExposureExceeded {
            limit: limits.tenant_daily_exposure_limit.to_string(),
            projected: projected.to_string(),
        });
    }
    if exposure.orders_in_window >= i64::from(limits.velocity_max_orders) {
        return Err(RiskBreach::VelocityExceeded {
            max_orders: limits.velocity_max_orders,
            window_secs: limits.velocity_window_secs,
        });
    }
    if !limits.allowed_assets.is_empty() && !limits.allowed_assets.contains(&asset) {
        return Err(RiskBreach::AssetNotAllowed { asset });
    }
    if limits.denied_assets.contains(&asset) {
        return Err(RiskBreach::AssetDenied { asset });
    }
    Ok(())
}

enum Verdict {
    Proceed,
    Held(RiskBreach),
}

/// Wraps a [`SwapEngine`] so that no hand-off reaches it without passing the
/// risk gate.
pub struct RiskGatedSwapEngine<E> {
    inner: E,
    pool: PgPool,
    limits: RiskLimits,
}

impl<E: SwapEngine> RiskGatedSwapEngine<E> {
    pub fn new(inner: E, pool: PgPool, limits: RiskLimits) -> Self {
        Self {
            inner,
            pool,
            limits,
        }
    }

    /// Evaluates and records the decision for `handoff` in one transaction.
    /// A decision already on record is replayed, so re-delivering a hand-off
    /// gets the same answer.
    async fn decide(&self, tenant_id: Uuid, handoff: &SwapHandoff) -> sqlx::Result<Verdict> {
        let now = Utc::now();
        let day_start = now.date_naive().and_time(NaiveTime::MIN).and_utc();
        let window_start = now - Duration::seconds(i64::from(self.limits.velocity_window_secs));

        let mut tx = self.pool.begin().await?;
        queries::lock_swap_risk_tenant(&mut tx, tenant_id).await?;

        if let Some((decision, breach)) =
            queries::get_swap_risk_decision(&mut tx, handoff.transaction_id).await?
        {
            return match decision.as_str() {
                "held" => {
                    let breach = breach
                        .ok_or_else(|| sqlx::Error::Protocol("held decision has no breach".into()))
                        .and_then(|v| {
                            serde_json::from_value::<RiskBreach>(v)
                                .map_err(|e| sqlx::Error::Decode(Box::new(e)))
                        })?;
                    Ok(Verdict::Held(breach))
                }
                _ => Ok(Verdict::Proceed),
            };
        }

        let exposure =
            queries::get_swap_risk_exposure(&mut tx, tenant_id, day_start, window_start).await?;
        let verdict = match evaluate(
            &self.limits,
            &handoff.source_asset,
            &handoff.amount,
            &exposure,
        ) {
            Ok(()) => Verdict::Proceed,
            Err(breach) => Verdict::Held(breach),
        };

        let breach_json = match &verdict {
            Verdict::Proceed => None,
            Verdict::Held(breach) => {
                Some(serde_json::to_value(breach).map_err(|e| sqlx::Error::Encode(Box::new(e)))?)
            }
        };
        let decision = if breach_json.is_some() {
            "held"
        } else {
            "accepted"
        };
        queries::insert_swap_risk_decision(
            &mut tx,
            handoff.transaction_id,
            tenant_id,
            &handoff.source_asset,
            &handoff.amount,
            decision,
            breach_json.as_ref(),
        )
        .await?;
        tx.commit().await?;

        Ok(verdict)
    }
}

#[async_trait]
impl<E: SwapEngine> SwapEngine for RiskGatedSwapEngine<E> {
    fn name(&self) -> &str {
        self.inner.name()
    }

    async fn on_swap_ready(&self, handoff: SwapHandoff) -> Result<SwapOutcome, SwapError> {
        // Tenant limits need a tenant to count against, so an unattributed
        // hand-off is not routed.
        let tenant_id = handoff.tenant_id.ok_or_else(|| {
            SwapError::InvalidHandoff("no tenant on hand-off; risk limits cannot be applied".into())
        })?;

        match self.decide(tenant_id, &handoff).await {
            Ok(Verdict::Proceed) => self.inner.on_swap_ready(handoff).await,
            Ok(Verdict::Held(breach)) => {
                AlertPayload::new(
                    names::SWAP_ORDER_HELD,
                    AlertSeverity::Warning,
                    format!("swap order held by risk gate: {breach}"),
                )
                .with_label("tenant_id", tenant_id)
                .with_label("transaction_id", handoff.transaction_id)
                .emit();
                Ok(SwapOutcome::Held { reason: breach })
            }
            Err(err) => {
                tracing::error!(
                    error = %err,
                    transaction_id = %handoff.transaction_id,
                    "swap risk gate could not evaluate order; not routing (fail closed)"
                );
                Err(SwapError::Unavailable)
            }
        }
    }
}
