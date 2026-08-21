/// Refactored webhook handlers for payments module
/// 
/// This module provides improved code structure for webhook handling with:
/// - Clear separation of concerns
/// - Reusable validation functions
/// - Better error handling
/// - Comprehensive test coverage

use crate::db::models::Transaction;
use crate::db::queries;
use crate::error::AppError;
use crate::validation::{
    sanitize_string, validate_asset_code, validate_max_len, validate_positive_amount,
    validate_stellar_address, AMOUNT_INPUT_MAX_LEN, ANCHOR_TRANSACTION_ID_MAX_LEN,
    CALLBACK_STATUS_MAX_LEN, CALLBACK_TYPE_MAX_LEN,
};
use crate::AppState;
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::types::BigDecimal;
use std::str::FromStr;
use tracing::instrument;
use utoipa::ToSchema;

/// Request payload for webhook transaction
#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct WebhookTransactionRequest {
    pub stellar_address: String,
    pub amount: String,
    pub asset_code: String,
    pub anchor_transaction_id: Option<String>,
    pub callback_type: Option<String>,
    pub callback_status: Option<String>,
}

/// Response payload for webhook transaction
#[derive(Debug, Serialize)]
pub struct WebhookTransactionResponse {
    pub id: String,
    pub status: String,
}

/// Validated webhook transaction (internal representation)
struct ValidatedWebhookTransaction {
    stellar_address: String,
    amount: BigDecimal,
    asset_code: String,
    anchor_transaction_id: Option<String>,
    callback_type: Option<String>,
    callback_status: Option<String>,
}

/// Sanitize optional string fields
fn sanitize_optional(value: Option<String>) -> Option<String> {
    value
        .map(|v| sanitize_string(&v))
        .and_then(|v| if v.is_empty() { None } else { Some(v) })
}

/// Validate stellar address field
fn validate_stellar_address_field(address: &str) -> Result<(), AppError> {
    validate_stellar_address(address)
        .map_err(|err| AppError::Validation(err.to_string()))
}

/// Validate asset code field
fn validate_asset_code_field(code: &str) -> Result<(), AppError> {
    validate_asset_code(code)
        .map_err(|err| AppError::Validation(err.to_string()))
}

/// Validate amount field
fn validate_amount_field(amount_str: &str) -> Result<BigDecimal, AppError> {
    validate_max_len("amount", amount_str, AMOUNT_INPUT_MAX_LEN)
        .map_err(|err| AppError::Validation(err.to_string()))?;

    let amount = amount_str
        .parse::<BigDecimal>()
        .map_err(|_| AppError::Validation("amount: must be a valid decimal".to_string()))?;

    validate_positive_amount(&amount)
        .map_err(|err| AppError::Validation(err.to_string()))?;

    Ok(amount)
}

/// Validate optional anchor transaction ID
fn validate_anchor_transaction_id_field(id: &Option<String>) -> Result<(), AppError> {
    if let Some(id) = id {
        validate_max_len("anchor_transaction_id", id, ANCHOR_TRANSACTION_ID_MAX_LEN)
            .map_err(|err| AppError::Validation(err.to_string()))?;
    }
    Ok(())
}

/// Validate optional callback type
fn validate_callback_type_field(callback_type: &Option<String>) -> Result<(), AppError> {
    if let Some(ct) = callback_type {
        validate_max_len("callback_type", ct, CALLBACK_TYPE_MAX_LEN)
            .map_err(|err| AppError::Validation(err.to_string()))?;
    }
    Ok(())
}

/// Validate optional callback status
fn validate_callback_status_field(callback_status: &Option<String>) -> Result<(), AppError> {
    if let Some(cs) = callback_status {
        validate_max_len("callback_status", cs, CALLBACK_STATUS_MAX_LEN)
            .map_err(|err| AppError::Validation(err.to_string()))?;
    }
    Ok(())
}

/// Validate and sanitize webhook payload
pub fn validate_webhook_payload(
    payload: WebhookTransactionRequest,
) -> Result<ValidatedWebhookTransaction, AppError> {
    let stellar_address = sanitize_string(&payload.stellar_address);
    let asset_code = sanitize_string(&payload.asset_code);
    let amount_str = sanitize_string(&payload.amount);
    let anchor_transaction_id = sanitize_optional(payload.anchor_transaction_id);
    let callback_type = sanitize_optional(payload.callback_type);
    let callback_status = sanitize_optional(payload.callback_status);

    // Validate all fields
    validate_stellar_address_field(&stellar_address)?;
    validate_asset_code_field(&asset_code)?;
    let amount = validate_amount_field(&amount_str)?;
    validate_anchor_transaction_id_field(&anchor_transaction_id)?;
    validate_callback_type_field(&callback_type)?;
    validate_callback_status_field(&callback_status)?;

    Ok(ValidatedWebhookTransaction {
        stellar_address,
        amount,
        asset_code,
        anchor_transaction_id,
        callback_type,
        callback_status,
    })
}

/// Handle webhook transaction callback
#[instrument(name = "webhook.transaction_callback", skip(state, payload))]
pub async fn transaction_callback(
    State(state): State<AppState>,
    Json(payload): Json<WebhookTransactionRequest>,
) -> Result<impl IntoResponse, AppError> {
    // Validate and sanitize all inputs before any DB interaction
    let payload = validate_webhook_payload(payload)?;

    let tx = Transaction::new(
        payload.stellar_address,
        payload.amount,
        payload.asset_code,
        payload.anchor_transaction_id,
        payload.callback_type,
        payload.callback_status,
        None, // memo
        None, // memo_type
        None, // metadata
    );

    let inserted = queries::insert_transaction(&state.db, &tx).await?;

    Ok((
        StatusCode::CREATED,
        Json(WebhookTransactionResponse {
            id: inserted.id.to_string(),
            status: inserted.status,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_payload() -> WebhookTransactionRequest {
        WebhookTransactionRequest {
            stellar_address: "G".to_owned() + &"A".repeat(55),
            amount: "42.50".to_string(),
            asset_code: "USD".to_string(),
            anchor_transaction_id: Some("anchor-1".to_string()),
            callback_type: Some("deposit".to_string()),
            callback_status: Some("completed".to_string()),
        }
    }

    #[test]
    fn test_validate_webhook_payload_accepts_valid_input() {
        let result = validate_webhook_payload(valid_payload());
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_webhook_payload_rejects_invalid_stellar_address() {
        let mut payload = valid_payload();
        payload.stellar_address = "BAD".to_string();
        assert!(validate_webhook_payload(payload).is_err());
    }

    #[test]
    fn test_validate_webhook_payload_rejects_invalid_asset_code() {
        let mut payload = valid_payload();
        payload.asset_code = "usd".to_string();
        assert!(validate_webhook_payload(payload).is_err());
    }

    #[test]
    fn test_validate_webhook_payload_rejects_negative_amount() {
        let mut payload = valid_payload();
        payload.amount = "-10.00".to_string();
        assert!(validate_webhook_payload(payload).is_err());
    }

    #[test]
    fn test_validate_webhook_payload_rejects_zero_amount() {
        let mut payload = valid_payload();
        payload.amount = "0".to_string();
        assert!(validate_webhook_payload(payload).is_err());
    }

    #[test]
    fn test_validate_webhook_payload_rejects_invalid_amount() {
        let mut payload = valid_payload();
        payload.amount = "not_a_number".to_string();
        assert!(validate_webhook_payload(payload).is_err());
    }

    #[test]
    fn test_validate_webhook_payload_accepts_optional_fields() {
        let mut payload = valid_payload();
        payload.anchor_transaction_id = None;
        payload.callback_type = None;
        payload.callback_status = None;
        assert!(validate_webhook_payload(payload).is_ok());
    }

    #[test]
    fn test_sanitize_optional_empty_string() {
        let result = sanitize_optional(Some("   ".to_string()));
        assert_eq!(result, None);
    }

    #[test]
    fn test_sanitize_optional_valid_string() {
        let result = sanitize_optional(Some("valid".to_string()));
        assert_eq!(result, Some("valid".to_string()));
    }

    #[test]
    fn test_sanitize_optional_none() {
        let result = sanitize_optional(None);
        assert_eq!(result, None);
    }

    #[test]
    fn test_validate_stellar_address_field_valid() {
        let address = "G".to_owned() + &"A".repeat(55);
        assert!(validate_stellar_address_field(&address).is_ok());
    }

    #[test]
    fn test_validate_stellar_address_field_invalid() {
        assert!(validate_stellar_address_field("INVALID").is_err());
    }

    #[test]
    fn test_validate_asset_code_field_valid() {
        assert!(validate_asset_code_field("USD").is_ok());
    }

    #[test]
    fn test_validate_asset_code_field_invalid() {
        assert!(validate_asset_code_field("usd").is_err());
    }

    #[test]
    fn test_validate_amount_field_valid() {
        let result = validate_amount_field("42.50");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_amount_field_invalid() {
        let result = validate_amount_field("not_a_number");
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_anchor_transaction_id_field_valid() {
        assert!(validate_anchor_transaction_id_field(&Some("anchor-1".to_string())).is_ok());
    }

    #[test]
    fn test_validate_anchor_transaction_id_field_none() {
        assert!(validate_anchor_transaction_id_field(&None).is_ok());
    }

    #[test]
    fn test_validate_callback_type_field_valid() {
        assert!(validate_callback_type_field(&Some("deposit".to_string())).is_ok());
    }

    #[test]
    fn test_validate_callback_type_field_none() {
        assert!(validate_callback_type_field(&None).is_ok());
    }

    #[test]
    fn test_validate_callback_status_field_valid() {
        assert!(validate_callback_status_field(&Some("completed".to_string())).is_ok());
    }

    #[test]
    fn test_validate_callback_status_field_none() {
        assert!(validate_callback_status_field(&None).is_ok());
    }
}
