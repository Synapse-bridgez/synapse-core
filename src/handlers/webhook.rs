use crate::AppState;
use crate::db::{models::Transaction, queries};
use crate::error::AppError;
use crate::validation::{
    AMOUNT_INPUT_MAX_LEN, ANCHOR_TRANSACTION_ID_MAX_LEN, CALLBACK_STATUS_MAX_LEN,
    CALLBACK_TYPE_MAX_LEN, sanitize_string, validate_asset_code, validate_max_len,
    validate_positive_amount, validate_stellar_address,
};
use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use sqlx::types::BigDecimal;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebhookTransactionRequest {
    pub stellar_address: String,
    pub amount: String,
    pub asset_code: String,
    pub anchor_transaction_id: Option<String>,
    pub callback_type: Option<String>,
    pub callback_status: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WebhookTransactionResponse {
    pub id: String,
    pub status: String,
}

struct ValidatedWebhookTransaction {
    stellar_address: String,
    amount: BigDecimal,
    asset_code: String,
    anchor_transaction_id: Option<String>,
    callback_type: Option<String>,
    callback_status: Option<String>,
}

fn sanitize_optional(value: Option<String>) -> Option<String> {
    value
        .map(|v| sanitize_string(&v))
        .and_then(|v| if v.is_empty() { None } else { Some(v) })
}

fn validate_webhook_payload(
    payload: WebhookTransactionRequest,
) -> Result<ValidatedWebhookTransaction, AppError> {
    let stellar_address = sanitize_string(&payload.stellar_address);
    let asset_code = sanitize_string(&payload.asset_code);
    let amount_str = sanitize_string(&payload.amount);
    let anchor_transaction_id = sanitize_optional(payload.anchor_transaction_id);
    let callback_type = sanitize_optional(payload.callback_type);
    let callback_status = sanitize_optional(payload.callback_status);

    validate_stellar_address(&stellar_address)
        .map_err(|err| AppError::Validation(err.to_string()))?;
    validate_asset_code(&asset_code).map_err(|err| AppError::Validation(err.to_string()))?;
    validate_max_len("amount", &amount_str, AMOUNT_INPUT_MAX_LEN)
        .map_err(|err| AppError::Validation(err.to_string()))?;
    if let Some(anchor_transaction_id) = &anchor_transaction_id {
        validate_max_len(
            "anchor_transaction_id",
            anchor_transaction_id,
            ANCHOR_TRANSACTION_ID_MAX_LEN,
        )
        .map_err(|err| AppError::Validation(err.to_string()))?;
    }
    if let Some(callback_type) = &callback_type {
        validate_max_len("callback_type", callback_type, CALLBACK_TYPE_MAX_LEN)
            .map_err(|err| AppError::Validation(err.to_string()))?;
    }
    if let Some(callback_status) = &callback_status {
        validate_max_len("callback_status", callback_status, CALLBACK_STATUS_MAX_LEN)
            .map_err(|err| AppError::Validation(err.to_string()))?;
    }

    let amount = amount_str
        .parse::<BigDecimal>()
        .map_err(|_| AppError::Validation("amount: must be a valid decimal".to_string()))?;
    validate_positive_amount(&amount).map_err(|err| AppError::Validation(err.to_string()))?;

    Ok(ValidatedWebhookTransaction {
        stellar_address,
        amount,
        asset_code,
        anchor_transaction_id,
        callback_type,
        callback_status,
    })
}

pub async fn transaction_callback(
    State(state): State<AppState>,
    Json(payload): Json<WebhookTransactionRequest>,
) -> Result<impl IntoResponse, AppError> {
    // Validate and sanitize all inputs before any DB interaction.
    let payload = validate_webhook_payload(payload)?;

    let tx = Transaction::new(
        payload.stellar_address,
        payload.amount,
        payload.asset_code,
        payload.anchor_transaction_id,
        payload.callback_type,
        payload.callback_status,
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
    fn webhook_payload_rejects_unknown_fields() {
        let raw = r#"{
            "stellar_address":"GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "amount":"10",
            "asset_code":"USD",
            "unknown":"x"
        }"#;

        let parsed = serde_json::from_str::<WebhookTransactionRequest>(raw);
        assert!(parsed.is_err());
    }

    #[test]
    fn validate_webhook_payload_accepts_valid_input() {
        let parsed = validate_webhook_payload(valid_payload());
        assert!(parsed.is_ok());
    }

    #[test]
    fn validate_webhook_payload_rejects_invalid_stellar_address() {
        let mut payload = valid_payload();
        payload.stellar_address = "BAD".to_string();

        let parsed = validate_webhook_payload(payload);
        assert!(parsed.is_err());
    }

    #[test]
    fn validate_webhook_payload_rejects_invalid_asset_code() {
        let mut payload = valid_payload();
        payload.asset_code = "usd".to_string();

        let parsed = validate_webhook_payload(payload);
        assert!(parsed.is_err());
    }

    #[test]
    fn validate_webhook_payload_rejects_invalid_amount() {
        let mut payload = valid_payload();
        payload.amount = "-1".to_string();

        let parsed = validate_webhook_payload(payload);
        assert!(parsed.is_err());
    }

    #[test]
    fn validate_webhook_payload_rejects_empty_required_fields() {
        let mut payload = valid_payload();
        payload.stellar_address = "   ".to_string();
        payload.amount = "   ".to_string();
        payload.asset_code = "   ".to_string();

        let parsed = validate_webhook_payload(payload);
        assert!(parsed.is_err());
    }

    #[test]
    fn validate_webhook_payload_rejects_unicode_in_validated_fields() {
        let mut payload = valid_payload();
        payload.stellar_address = format!("G{}", "Ä".repeat(55));

        let parsed = validate_webhook_payload(payload);
        assert!(parsed.is_err());

        let mut payload = valid_payload();
        payload.asset_code = "USÐ".to_string();

        let parsed = validate_webhook_payload(payload);
        assert!(parsed.is_err());
    }

    #[test]
    fn validate_webhook_payload_rejects_sql_injection_like_strings() {
        let mut payload = valid_payload();
        payload.asset_code = "USD'; DROP TABLE transactions; --".to_string();

        let parsed = validate_webhook_payload(payload);
        assert!(parsed.is_err());

        let mut payload = valid_payload();
        payload.amount = "1; DROP TABLE transactions; --".to_string();

        let parsed = validate_webhook_payload(payload);
        assert!(parsed.is_err());
    }

    #[test]
    fn validate_webhook_payload_sanitizes_control_characters_in_optional_fields() {
        let mut payload = valid_payload();
        payload.anchor_transaction_id = Some("abc\u{0000}123\u{0007}".to_string());
        payload.callback_type = Some("dep\u{0001}osit".to_string());
        payload.callback_status = Some("comple\u{0002}ted".to_string());

        let parsed = validate_webhook_payload(payload).expect("payload should be valid");
        assert_eq!(parsed.anchor_transaction_id.as_deref(), Some("abc123"));
        assert_eq!(parsed.callback_type.as_deref(), Some("deposit"));
        assert_eq!(parsed.callback_status.as_deref(), Some("completed"));
    }

    #[test]
    fn validate_webhook_payload_rejects_overlong_optional_fields() {
        let mut payload = valid_payload();
        payload.anchor_transaction_id = Some("a".repeat(256));
        assert!(validate_webhook_payload(payload).is_err());

        let mut payload = valid_payload();
        payload.callback_type = Some("a".repeat(21));
        assert!(validate_webhook_payload(payload).is_err());

        let mut payload = valid_payload();
        payload.callback_status = Some("a".repeat(21));
        assert!(validate_webhook_payload(payload).is_err());
    }
}
