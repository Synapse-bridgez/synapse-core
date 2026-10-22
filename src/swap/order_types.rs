//! Conditional swap order types (limit, stop-loss, take-profit).
//!
//! Trigger state is persisted (`swap_orders`, see
//! `migrations/20260928000001_swap_controls.sql`); nothing here is held only in
//! memory. A triggered order is promoted to `pending` and then follows the same
//! idempotent path as a REST-submitted market order, so the quote's slippage
//! protection and minimum-received check apply unchanged at execution time.
//! A quote that expired while the order waited is re-quoted at promotion; if the
//! re-quote violates `limit_price`/min-received the order is `expired`, never
//! silently dropped.

use chrono::{DateTime, Duration, Utc};
use sqlx::types::BigDecimal;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderType {
    Market,
    Limit,
    StopLoss,
    TakeProfit,
}

impl OrderType {
    pub fn as_str(&self) -> &'static str {
        match self {
            OrderType::Market => "market",
            OrderType::Limit => "limit",
            OrderType::StopLoss => "stop_loss",
            OrderType::TakeProfit => "take_profit",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerDirection {
    /// Fire when price >= trigger_price.
    Above,
    /// Fire when price <= trigger_price.
    Below,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeInForce {
    /// Good-till-cancelled.
    Gtc,
    /// Expires `ttl` after the order triggers if not filled.
    Ioc,
    /// Expires at the order's `expires_at`.
    Gtd,
}

/// Lifecycle of a conditional order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderStatus {
    /// Waiting for the trigger condition.
    Open,
    /// Trigger fired; promoted to the executable path.
    Triggered,
    Filled,
    Cancelled,
    Expired,
}

impl OrderStatus {
    pub fn can_transition_to(self, next: OrderStatus) -> bool {
        use OrderStatus::*;
        matches!(
            (self, next),
            (Open, Triggered)
                | (Open, Cancelled)
                | (Open, Expired)
                | (Triggered, Filled)
                | (Triggered, Expired)
        )
    }

    /// Only untriggered orders may be cancelled or amended.
    pub fn is_amendable(self) -> bool {
        self == OrderStatus::Open
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConditionalOrder {
    pub order_type: OrderType,
    pub trigger_price: Option<BigDecimal>,
    pub trigger_direction: Option<TriggerDirection>,
    pub limit_price: Option<BigDecimal>,
    pub time_in_force: TimeInForce,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrderValidationError {
    MissingTrigger,
    UnexpectedTrigger,
    DirectionMismatch,
    NonPositivePrice,
    MissingExpiry,
}

impl ConditionalOrder {
    /// Check field consistency for the order type. Stop-loss fires on a fall,
    /// take-profit on a rise; limit orders may use either direction.
    pub fn validate(&self) -> Result<(), OrderValidationError> {
        let positive = |p: &BigDecimal| *p > BigDecimal::from(0);
        if self.order_type == OrderType::Market {
            if self.trigger_price.is_some() || self.trigger_direction.is_some() {
                return Err(OrderValidationError::UnexpectedTrigger);
            }
            return Ok(());
        }
        let (Some(tp), Some(dir)) = (&self.trigger_price, self.trigger_direction) else {
            return Err(OrderValidationError::MissingTrigger);
        };
        if !positive(tp) || self.limit_price.as_ref().is_some_and(|l| !positive(l)) {
            return Err(OrderValidationError::NonPositivePrice);
        }
        match (self.order_type, dir) {
            (OrderType::StopLoss, TriggerDirection::Above)
            | (OrderType::TakeProfit, TriggerDirection::Below) => {
                return Err(OrderValidationError::DirectionMismatch)
            }
            _ => {}
        }
        if self.time_in_force == TimeInForce::Gtd && self.expires_at.is_none() {
            return Err(OrderValidationError::MissingExpiry);
        }
        Ok(())
    }
}

/// Expiry deadline for a triggered order that has not filled.
pub fn triggered_deadline(
    tif: TimeInForce,
    triggered_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
    ioc_ttl: Duration,
) -> Option<DateTime<Utc>> {
    match tif {
        TimeInForce::Gtc => None,
        TimeInForce::Ioc => Some(triggered_at + ioc_ttl),
        TimeInForce::Gtd => expires_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn bd(s: &str) -> BigDecimal {
        BigDecimal::from_str(s).unwrap()
    }

    fn order(t: OrderType, d: Option<TriggerDirection>) -> ConditionalOrder {
        ConditionalOrder {
            order_type: t,
            trigger_price: Some(bd("1.5")),
            trigger_direction: d,
            limit_price: None,
            time_in_force: TimeInForce::Gtc,
            expires_at: None,
        }
    }

    #[test]
    fn stop_loss_requires_below() {
        assert!(order(OrderType::StopLoss, Some(TriggerDirection::Below))
            .validate()
            .is_ok());
        assert_eq!(
            order(OrderType::StopLoss, Some(TriggerDirection::Above)).validate(),
            Err(OrderValidationError::DirectionMismatch)
        );
    }

    #[test]
    fn take_profit_requires_above() {
        assert_eq!(
            order(OrderType::TakeProfit, Some(TriggerDirection::Below)).validate(),
            Err(OrderValidationError::DirectionMismatch)
        );
    }

    #[test]
    fn missing_trigger_and_market_rules() {
        assert_eq!(
            order(OrderType::Limit, None).validate(),
            Err(OrderValidationError::MissingTrigger)
        );
        assert_eq!(
            order(OrderType::Market, Some(TriggerDirection::Above)).validate(),
            Err(OrderValidationError::UnexpectedTrigger)
        );
    }

    #[test]
    fn gtd_needs_expiry() {
        let mut o = order(OrderType::Limit, Some(TriggerDirection::Below));
        o.time_in_force = TimeInForce::Gtd;
        assert_eq!(o.validate(), Err(OrderValidationError::MissingExpiry));
    }

    #[test]
    fn status_transitions() {
        assert!(OrderStatus::Open.can_transition_to(OrderStatus::Triggered));
        assert!(!OrderStatus::Filled.can_transition_to(OrderStatus::Open));
        assert!(!OrderStatus::Triggered.can_transition_to(OrderStatus::Cancelled));
        assert!(OrderStatus::Open.is_amendable());
        assert!(!OrderStatus::Triggered.is_amendable());
    }

    #[test]
    fn deadlines() {
        let now = Utc::now();
        assert_eq!(
            triggered_deadline(TimeInForce::Gtc, now, None, Duration::seconds(30)),
            None
        );
        assert_eq!(
            triggered_deadline(TimeInForce::Ioc, now, None, Duration::seconds(30)),
            Some(now + Duration::seconds(30))
        );
    }
}
