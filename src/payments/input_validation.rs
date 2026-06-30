//! Input validation for the payments / settlement module.
//!
//! Centralises all settlement-specific validation so that handlers and
//! services can call a single function and receive a typed error instead of
//! scattering ad-hoc string checks throughout the codebase.

use bigdecimal::BigDecimal;
use once_cell::sync::Lazy;
use std::collections::HashSet;

use crate::payments::error::PaymentError;

/// Maximum number of decimal places accepted for a payment amount.
const MAX_DECIMAL_PLACES: u64 = 7;

/// Minimum allowed payment amount (exclusive lower bound is zero, but we also
/// enforce a practical floor to avoid dust transactions).
const MIN_AMOUNT: &str = "0.0000001";

/// Maximum allowed payment amount to guard against obvious data-entry errors.
const MAX_AMOUNT: &str = "1000000000";

/// Allowed settlement status values.
static ALLOWED_STATUSES: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    [
        "pending",
        "completed",
        "pending_review",
        "disputed",
        "adjusted",
        "voided",
    ]
    .into_iter()
    .collect()
});

/// Validates a payment amount string and returns the parsed [`BigDecimal`].
///
/// Rules:
/// - Must be parseable as a decimal number.
/// - Must be strictly positive.
/// - Must not exceed [`MAX_DECIMAL_PLACES`] fractional digits.
/// - Must be within `[MIN_AMOUNT, MAX_AMOUNT]`.
pub fn validate_payment_amount(raw: &str) -> Result<BigDecimal, PaymentError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(PaymentError::InvalidAmount(
            "amount must not be empty".into(),
        ));
    }

    let amount: BigDecimal = trimmed
        .parse()
        .map_err(|_| PaymentError::InvalidAmount(format!("'{trimmed}' is not a valid decimal")))?;

    if amount <= BigDecimal::from(0) {
        return Err(PaymentError::InvalidAmount(
            "amount must be greater than zero".into(),
        ));
    }

    // Range checks come before the precision check so that an out-of-range
    // amount is reported as below-minimum / above-maximum rather than as a
    // precision error (values below the minimum necessarily carry more
    // fractional digits than allowed).
    let min: BigDecimal = MIN_AMOUNT.parse().unwrap();
    let max: BigDecimal = MAX_AMOUNT.parse().unwrap();

    if amount < min {
        return Err(PaymentError::AmountBelowMinimum(format!(
            "amount {amount} is below the minimum {MIN_AMOUNT}"
        )));
    }

    if amount > max {
        return Err(PaymentError::InvalidAmount(format!(
            "amount {amount} exceeds the maximum {MAX_AMOUNT}"
        )));
    }

    // Check decimal precision
    let (_, scale) = amount.as_bigint_and_exponent();
    if scale > MAX_DECIMAL_PLACES as i64 {
        return Err(PaymentError::InvalidAmount(format!(
            "amount must have at most {MAX_DECIMAL_PLACES} decimal places"
        )));
    }

    Ok(amount)
}

/// Validates a settlement status string.
pub fn validate_settlement_status(status: &str) -> Result<(), PaymentError> {
    let s = status.trim();
    if s.is_empty() {
        return Err(PaymentError::InvalidStatus(
            "status must not be empty".into(),
        ));
    }
    if !ALLOWED_STATUSES.contains(s) {
        return Err(PaymentError::InvalidStatus(format!(
            "'{s}' is not a recognised settlement status"
        )));
    }
    Ok(())
}

/// Validates an asset code for use in settlement operations.
///
/// Delegates to the shared validation layer so the rules stay in one place.
pub fn validate_settlement_asset_code(asset_code: &str) -> Result<(), PaymentError> {
    crate::validation::validate_asset_code(asset_code)
        .map_err(|e| PaymentError::InvalidAssetCode(e.message))
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- validate_payment_amount ---

    #[test]
    fn valid_amount_accepted() {
        let amount = validate_payment_amount("100.50").unwrap();
        assert_eq!(amount, "100.50".parse::<BigDecimal>().unwrap());
    }

    #[test]
    fn empty_amount_rejected() {
        assert!(matches!(
            validate_payment_amount(""),
            Err(PaymentError::InvalidAmount(_))
        ));
    }

    #[test]
    fn zero_amount_rejected() {
        assert!(matches!(
            validate_payment_amount("0"),
            Err(PaymentError::InvalidAmount(_))
        ));
    }

    #[test]
    fn negative_amount_rejected() {
        assert!(matches!(
            validate_payment_amount("-1.00"),
            Err(PaymentError::InvalidAmount(_))
        ));
    }

    #[test]
    fn non_numeric_amount_rejected() {
        assert!(matches!(
            validate_payment_amount("abc"),
            Err(PaymentError::InvalidAmount(_))
        ));
    }

    #[test]
    fn amount_below_minimum_rejected() {
        assert!(matches!(
            validate_payment_amount("0.00000001"),
            Err(PaymentError::AmountBelowMinimum(_))
        ));
    }

    #[test]
    fn amount_above_maximum_rejected() {
        assert!(matches!(
            validate_payment_amount("1000000001"),
            Err(PaymentError::InvalidAmount(_))
        ));
    }

    #[test]
    fn too_many_decimal_places_rejected() {
        // 8 decimal places — one more than allowed
        assert!(matches!(
            validate_payment_amount("1.00000001"),
            Err(PaymentError::InvalidAmount(_))
        ));
    }

    #[test]
    fn whitespace_trimmed_before_validation() {
        assert!(validate_payment_amount("  50.00  ").is_ok());
    }

    // --- validate_settlement_status ---

    #[test]
    fn valid_statuses_accepted() {
        for s in &[
            "pending",
            "completed",
            "disputed",
            "voided",
            "adjusted",
            "pending_review",
        ] {
            assert!(
                validate_settlement_status(s).is_ok(),
                "status '{s}' should be valid"
            );
        }
    }

    #[test]
    fn unknown_status_rejected() {
        assert!(matches!(
            validate_settlement_status("unknown"),
            Err(PaymentError::InvalidStatus(_))
        ));
    }

    #[test]
    fn empty_status_rejected() {
        assert!(matches!(
            validate_settlement_status(""),
            Err(PaymentError::InvalidStatus(_))
        ));
    }

    // --- validate_settlement_asset_code ---

    #[test]
    fn valid_asset_code_accepted() {
        assert!(validate_settlement_asset_code("USD").is_ok());
    }

    #[test]
    fn invalid_asset_code_rejected() {
        assert!(validate_settlement_asset_code("eur").is_err());
        assert!(validate_settlement_asset_code("").is_err());
    }
}
