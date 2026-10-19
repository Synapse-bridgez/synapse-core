//! Swap quotes: strict request validation, pricing and risk checks.
//!
//! Pricing here is a flat-fee 1:1 placeholder (`FEE_BPS`) until a routing
//! engine is wired behind the [`SwapEngine`](super::SwapEngine) boundary; the
//! API contract (fields, errors, expiry) does not depend on that.

use chrono::{DateTime, Duration, Utc};
use sqlx::types::BigDecimal;
use sqlx::PgPool;
use std::str::FromStr;
use uuid::Uuid;

use crate::error::AppError;

/// How long a quote can be turned into an order.
pub const QUOTE_TTL_SECS: i64 = 30;
/// Flat fee applied to every quote, in basis points.
pub const FEE_BPS: i32 = 30;
/// Maximum decimal places accepted for an amount (Stellar precision).
pub const MAX_AMOUNT_SCALE: i64 = 7;
/// Per-quote risk ceiling on the source amount.
const MAX_SOURCE_AMOUNT: &str = "1000000";

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SwapQuote {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub source_asset: String,
    pub dest_asset: String,
    pub source_amount: BigDecimal,
    pub dest_amount: BigDecimal,
    pub rate: BigDecimal,
    pub fee_bps: i32,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

impl SwapQuote {
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at <= now
    }
}

/// Validates an asset identifier: `CODE` (1-12 alphanumerics) or
/// `CODE:ISSUER` where ISSUER is a 56-character Stellar account (`G...`).
pub fn validate_asset(field: &str, asset: &str) -> Result<(), AppError> {
    let (code, issuer) = match asset.split_once(':') {
        Some((c, i)) => (c, Some(i)),
        None => (asset, None),
    };
    if code.is_empty() || code.len() > 12 || !code.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(AppError::Validation(format!(
            "{field} must be an asset code of 1-12 alphanumeric characters"
        )));
    }
    if let Some(issuer) = issuer {
        let ok = issuer.len() == 56
            && issuer.starts_with('G')
            && issuer
                .chars()
                .all(|c| c.is_ascii_uppercase() || ('2'..='7').contains(&c));
        if !ok {
            return Err(AppError::Validation(format!(
                "{field} has an invalid issuer account"
            )));
        }
    }
    Ok(())
}

/// Parses a positive decimal amount with at most [`MAX_AMOUNT_SCALE`] places.
pub fn parse_amount(field: &str, raw: &str) -> Result<BigDecimal, AppError> {
    let well_formed = !raw.is_empty()
        && raw.len() <= 40
        && raw.chars().all(|c| c.is_ascii_digit() || c == '.')
        && raw.matches('.').count() <= 1
        && !raw.starts_with('.')
        && !raw.ends_with('.');
    if !well_formed {
        return Err(AppError::Validation(format!(
            "{field} must be a positive decimal string"
        )));
    }
    let amount = BigDecimal::from_str(raw)
        .map_err(|_| AppError::Validation(format!("{field} must be a positive decimal string")))?;
    if amount <= BigDecimal::from(0) {
        return Err(AppError::Validation(format!("{field} must be positive")));
    }
    let fractional_digits = raw.split_once('.').map_or(0, |(_, f)| f.len() as i64);
    if fractional_digits > MAX_AMOUNT_SCALE {
        return Err(AppError::Validation(format!(
            "{field} supports at most {MAX_AMOUNT_SCALE} decimal places"
        )));
    }
    Ok(amount)
}

/// Risk gate shared by quote and order creation. Returns only a stable
/// reason code; thresholds are never reported to the caller.
pub fn check_risk(source_amount: &BigDecimal) -> Result<(), AppError> {
    let max = BigDecimal::from_str(MAX_SOURCE_AMOUNT).expect("valid constant");
    if *source_amount > max {
        return Err(AppError::SwapRiskRejected("amount_limit_exceeded"));
    }
    Ok(())
}

/// Validates the request, prices it and persists a tenant-owned quote.
pub async fn create_quote(
    pool: &PgPool,
    tenant_id: Uuid,
    source_asset: &str,
    dest_asset: &str,
    source_amount: &str,
) -> Result<SwapQuote, AppError> {
    validate_asset("source_asset", source_asset)?;
    validate_asset("dest_asset", dest_asset)?;
    if source_asset == dest_asset {
        return Err(AppError::Validation(
            "source_asset and dest_asset must differ".to_string(),
        ));
    }
    let source_amount = parse_amount("source_amount", source_amount)?;
    check_risk(&source_amount)?;

    let rate = BigDecimal::from(10_000 - FEE_BPS) / BigDecimal::from(10_000);
    let dest_amount = (&source_amount * &rate).with_scale(MAX_AMOUNT_SCALE);
    if dest_amount <= BigDecimal::from(0) {
        return Err(AppError::Validation(
            "source_amount is too small to quote".to_string(),
        ));
    }

    let quote = SwapQuote {
        id: Uuid::new_v4(),
        tenant_id,
        source_asset: source_asset.to_string(),
        dest_asset: dest_asset.to_string(),
        source_amount,
        dest_amount,
        rate,
        fee_bps: FEE_BPS,
        expires_at: Utc::now() + Duration::seconds(QUOTE_TTL_SECS),
        created_at: Utc::now(),
    };
    crate::db::queries::insert_swap_quote(pool, &quote).await?;
    Ok(quote)
}
