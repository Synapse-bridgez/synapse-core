//! Trigger evaluation for conditional orders.
//!
//! [`evaluate`] is a pure function so the restartable worker loop (which reads
//! `status = 'open'` candidates ordered by the `(status, trigger_price)` index
//! and promotes them with a conditional `UPDATE ... WHERE status = 'open'`)
//! carries no in-memory trigger state.

use chrono::{DateTime, Duration, Utc};
use sqlx::types::BigDecimal;

use super::order_types::TriggerDirection;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriggerDecision {
    Fire,
    Hold,
    /// Market data is stale, from the future beyond clock skew, or missing:
    /// fail closed and do not trigger.
    StaleData,
}

/// Evaluate one order against the latest normalized price.
///
/// * exactly-at-trigger fires (`>=` / `<=`), which also covers a gap through
///   the trigger.
/// * `max_age` bounds data staleness; `max_skew` tolerates small clock skew
///   for prices stamped slightly in the future.
/// * `hysteresis` (>= 0) is applied by the caller to an already-armed order
///   only; the evaluator itself is stateless, and promotion is one-way, so an
///   order oscillating around the trigger cannot fire twice.
pub fn evaluate(
    direction: TriggerDirection,
    trigger_price: &BigDecimal,
    price: Option<&BigDecimal>,
    price_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    max_age: Duration,
    max_skew: Duration,
) -> TriggerDecision {
    let (Some(price), Some(price_at)) = (price, price_at) else {
        return TriggerDecision::StaleData;
    };
    if now - price_at > max_age || price_at - now > max_skew {
        return TriggerDecision::StaleData;
    }
    let hit = match direction {
        TriggerDirection::Above => price >= trigger_price,
        TriggerDirection::Below => price <= trigger_price,
    };
    if hit {
        TriggerDecision::Fire
    } else {
        TriggerDecision::Hold
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn bd(s: &str) -> BigDecimal {
        BigDecimal::from_str(s).unwrap()
    }

    fn run(dir: TriggerDirection, price: &str, age_secs: i64) -> TriggerDecision {
        let now = Utc::now();
        evaluate(
            dir,
            &bd("100"),
            Some(&bd(price)),
            Some(now - Duration::seconds(age_secs)),
            now,
            Duration::seconds(10),
            Duration::seconds(2),
        )
    }

    #[test]
    fn exactly_at_trigger_fires() {
        assert_eq!(
            run(TriggerDirection::Above, "100", 1),
            TriggerDecision::Fire
        );
        assert_eq!(
            run(TriggerDirection::Below, "100", 1),
            TriggerDecision::Fire
        );
    }

    #[test]
    fn gap_through_trigger_fires() {
        assert_eq!(
            run(TriggerDirection::Above, "130", 1),
            TriggerDecision::Fire
        );
        assert_eq!(run(TriggerDirection::Below, "70", 1), TriggerDecision::Fire);
    }

    #[test]
    fn not_reached_holds() {
        assert_eq!(run(TriggerDirection::Above, "99", 1), TriggerDecision::Hold);
        assert_eq!(
            run(TriggerDirection::Below, "101", 1),
            TriggerDecision::Hold
        );
    }

    #[test]
    fn stale_feed_fails_closed() {
        assert_eq!(
            run(TriggerDirection::Above, "130", 60),
            TriggerDecision::StaleData
        );
    }

    #[test]
    fn clock_skew_tolerated_up_to_limit() {
        assert_eq!(
            run(TriggerDirection::Above, "130", -1),
            TriggerDecision::Fire
        );
        assert_eq!(
            run(TriggerDirection::Above, "130", -30),
            TriggerDecision::StaleData
        );
    }

    #[test]
    fn missing_data_fails_closed() {
        let now = Utc::now();
        assert_eq!(
            evaluate(
                TriggerDirection::Above,
                &bd("1"),
                None,
                None,
                now,
                Duration::seconds(10),
                Duration::seconds(2)
            ),
            TriggerDecision::StaleData
        );
    }
}
