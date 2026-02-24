use crate::ApiState;
use crate::db::{models::Transaction, queries};
use crate::error::AppError;
use crate::validation::{
    AMOUNT_INPUT_MAX_LEN, ANCHOR_TRANSACTION_ID_MAX_LEN, CALLBACK_STATUS_MAX_LEN,
    CALLBACK_TYPE_MAX_LEN, sanitize_string, validate_asset_code, validate_max_len,
    validate_positive_amount, validate_stellar_address,
};
use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use crate::db::queries;
use crate::db::models::Transaction;
use crate::error::AppError;
use utoipa::ToSchema;
use std::str::FromStr;

#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct CallbackPayload {
    pub stellar_account: String,
    pub amount: String,
    pub asset_code: String,
    pub callback_type: Option<String>,
    pub callback_status: Option<String>,
    pub anchor_transaction_id: Option<String>,
    pub memo: Option<String>,
    pub memo_type: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct WebhookPayload {
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
#[derive(Debug, Serialize, ToSchema)]
pub struct WebhookResponse {
    pub success: bool,
    pub message: String,
}

fn validate_memo_type(memo_type: &Option<String>) -> Result<(), AppError> {
    if let Some(mt) = memo_type {
        match mt.as_str() {
            "text" | "hash" | "id" => Ok(()),
            _ => Err(AppError::Validation(format!(
                "Invalid memo_type '{}'. Must be one of: text, hash, id",
                mt
            ))),
        }
    } else {
        Ok(())
    }
}

#[utoipa::path(
    post,
    path = "/callback",
    request_body = CallbackPayload,
    responses(
        (status = 201, description = "Transaction created", body = crate::schemas::TransactionSchema),
        (status = 400, description = "Invalid payload"),
        (status = 500, description = "Processing error")
    ),
    tag = "Webhooks"
)]
pub async fn callback(
    State(state): State<ApiState>,
    Json(payload): Json<CallbackPayload>,
) -> Result<impl IntoResponse, AppError> {
    validate_memo_type(&payload.memo_type)?;

    let amount = sqlx::types::BigDecimal::from_str(&payload.amount)
        .map_err(|_| AppError::Validation(format!("Invalid amount: {}", payload.amount)))?;

    let tx = Transaction::new(
        payload.stellar_account,
        amount,
        payload.asset_code,
        payload.anchor_transaction_id,
        payload.callback_type,
        payload.callback_status,
        payload.memo,
        payload.memo_type,
        payload.metadata,
    );

    let inserted = queries::insert_transaction(&state.app_state.db, &tx).await
        .map_err(|e| AppError::DatabaseError(e.to_string()))?;

    Ok((StatusCode::CREATED, Json(inserted)))
}

#[utoipa::path(
    post,
    path = "/webhook",
    request_body = WebhookPayload,
    responses(
        (status = 200, description = "Webhook processed successfully", body = WebhookResponse),
        (status = 400, description = "Invalid payload"),
        (status = 500, description = "Processing error")
    ),
    tag = "Webhooks"
)]
pub async fn handle_webhook(
    State(_state): State<ApiState>,
    Json(payload): Json<WebhookPayload>,
) -> impl IntoResponse {
    tracing::info!("Processing webhook with id: {}", payload.id);

    let response = WebhookResponse {
        success: true,
        message: format!("Webhook {} processed successfully", payload.id),
    };

    (StatusCode::OK, Json(response))
}

/// Callback endpoint for transactions (placeholder)
pub async fn callback(State(_state): State<ApiState>) -> impl IntoResponse {
    StatusCode::NOT_IMPLEMENTED
}

/// Get a specific transaction
/// 
/// Returns details for a specific transaction by ID
#[utoipa::path(
    get,
    path = "/transactions/{id}",
    params(
        ("id" = String, Path, description = "Transaction ID")
    ),
    responses(
        (status = 200, description = "Transaction found", body = crate::schemas::TransactionSchema),
        (status = 404, description = "Transaction not found"),
        (status = 500, description = "Database error")
    ),
    tag = "Transactions"
)]
pub async fn get_transaction(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let transaction = queries::get_transaction(&state.app_state.db, id).await
        .map_err(|e| match e {
            sqlx::Error::RowNotFound => AppError::NotFound(format!("Transaction {} not found", id)),
            _ => AppError::DatabaseError(e.to_string()),
        })?;

    Ok(Json(transaction))
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub cursor: Option<String>,
    pub limit: Option<i64>,
    /// direction: "forward" (older items) or "backward" (newer items)
    pub direction: Option<String>,
}

#[utoipa::path(
    get,
    path = "/transactions",
    params(
        ("cursor" = Option<String>, Query, description = "Cursor for pagination"),
        ("limit" = Option<i64>, Query, description = "Page size"),
        ("direction" = Option<String>, Query, description = "forward or backward")
    ),
    responses(
        (status = 200, description = "List transactions with pagination metadata"),
        (status = 500, description = "Database error")
    ),
    tag = "Transactions"
)]
pub async fn list_transactions(
    State(state): State<AppState>,
    Query(params): Query<ListQuery>,
) -> Result<impl IntoResponse, AppError> {
    let limit = params.limit.unwrap_or(25).min(100);
    let backward = params.direction.as_deref() == Some("backward");

    let decoded_cursor = if let Some(ref c) = params.cursor {
        match cursor_util::decode(c) {
            Ok((ts, id)) => Some((ts, id)),
            Err(e) => return Err(AppError::BadRequest(format!("invalid cursor: {}", e))),
        }
    } else {
        None
    };

    // fetch one extra to determine has_more
    let fetch_limit = limit + 1;
    let mut rows = queries::list_transactions(&state.db, fetch_limit, decoded_cursor, backward)
        .await
        .map_err(|e| AppError::DatabaseError(e.to_string()))?;

    let has_more = rows.len() as i64 > limit;
    if has_more {
        rows.truncate(limit as usize);
    }

    // next cursor is the last item in the returned rows
    let next_cursor = rows.last().map(|r: &TxModel| cursor_util::encode(r.created_at, r.id));

    let resp = serde_json::json!({
        "data": rows,
        "meta": {
            "next_cursor": next_cursor,
            "has_more": has_more
        }
    });

    Ok(Json(resp))
}

/// Wrapper to accept the router's ApiState without forcing all handlers to change.
pub async fn list_transactions_api(
    State(api_state): State<crate::ApiState>,
    Query(params): Query<ListQuery>,
) -> Result<impl IntoResponse, AppError> {
    // forward to the AppState-based handler
    let app_state = api_state.app_state;
    // call the inner logic directly to avoid extractor conflicts
    let limit = params.limit.unwrap_or(25).min(100);
    let backward = params.direction.as_deref() == Some("backward");

    let decoded_cursor = if let Some(ref c) = params.cursor {
        match cursor_util::decode(c) {
            Ok((ts, id)) => Some((ts, id)),
            Err(e) => return Err(AppError::BadRequest(format!("invalid cursor: {}", e))),
        }
    } else {
        None
    };

    let fetch_limit = limit + 1;
    let mut rows = queries::list_transactions(&app_state.db, fetch_limit, decoded_cursor, backward)
        .await
        .map_err(|e| AppError::DatabaseError(e.to_string()))?;

    let has_more = rows.len() as i64 > limit;
    if has_more {
        rows.truncate(limit as usize);
    }

    let next_cursor = rows.last().map(|r: &TxModel| cursor_util::encode(r.created_at, r.id));

    let resp = serde_json::json!({
        "data": rows,
        "meta": {
            "next_cursor": next_cursor,
            "has_more": has_more
        }
    });

    Ok(Json(resp))
}
