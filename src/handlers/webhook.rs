use crate::{AppState, db::models::Transaction, error::AppError};
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use sqlx::types::BigDecimal;
use std::str::FromStr;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct CallbackPayload {
    pub id: String,
    pub amount_in: String,
    pub stellar_account: String,
    pub asset_code: String,
}

#[derive(Debug, Serialize)]
pub struct CallbackResponse {
    pub transaction_id: Uuid,
    pub status: String,
}

/// Validates the callback payload according to business rules
fn validate_payload(payload: &CallbackPayload) -> Result<(), AppError> {
    // Validate amount > 0
    let amount = BigDecimal::from_str(&payload.amount_in)
        .map_err(|_| AppError::Validation("Invalid amount format".to_string()))?;
    
    if amount <= BigDecimal::from(0) {
        return Err(AppError::Validation("Amount must be greater than 0".to_string()));
    }

    // Validate Stellar address length (56 characters for public key)
    if payload.stellar_account.len() != 56 {
        return Err(AppError::Validation(
            "Stellar account must be 56 characters".to_string()
        ));
    }

    // Validate Stellar address starts with 'G' (public key)
    if !payload.stellar_account.starts_with('G') {
        return Err(AppError::Validation(
            "Stellar account must start with 'G'".to_string()
        ));
    }

    // Validate asset code length (max 12 characters)
    if payload.asset_code.is_empty() || payload.asset_code.len() > 12 {
        return Err(AppError::Validation(
            "Asset code must be between 1 and 12 characters".to_string()
        ));
    }

    Ok(())
}

/// Handler for POST /callback/transaction
/// Receives fiat deposit events from Stellar Anchor Platform
pub async fn handle_callback(
    State(state): State<AppState>,
    Json(payload): Json<CallbackPayload>,
) -> Result<impl IntoResponse, AppError> {
    tracing::info!("Received callback: {:?}", payload);

    // Validate payload
    validate_payload(&payload)?;

    // Parse amount
    let amount = BigDecimal::from_str(&payload.amount_in)
        .map_err(|_| AppError::Validation("Invalid amount format".to_string()))?;

    // Create transaction
    let transaction = Transaction::new(
        payload.stellar_account.clone(),
        amount,
        payload.asset_code.clone(),
        Some(payload.id.clone()),
        Some("deposit".to_string()),
        Some("pending".to_string()),
    );

    // Insert into database
    let inserted = sqlx::query_as!(
        Transaction,
        r#"
        INSERT INTO transactions (
            id, stellar_account, amount, asset_code, status,
            created_at, updated_at, anchor_transaction_id, callback_type, callback_status
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
        RETURNING *
        "#,
        transaction.id,
        transaction.stellar_account,
        transaction.amount,
        transaction.asset_code,
        transaction.status,
        transaction.created_at,
        transaction.updated_at,
        transaction.anchor_transaction_id,
        transaction.callback_type,
        transaction.callback_status,
    )
    .fetch_one(&state.db)
    .await?;

    tracing::info!("Transaction persisted with ID: {}", inserted.id);

    let response = CallbackResponse {
        transaction_id: inserted.id,
        status: inserted.status,
    };

    Ok((StatusCode::CREATED, Json(response)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_payload_valid() {
        let payload = CallbackPayload {
            id: "anchor-123".to_string(),
            amount_in: "100.50".to_string(),
            stellar_account: "GABCDEFGHIJKLMNOPQRSTUVWXYZ234567890ABCDEFGHIJKLMNOPQR".to_string(),
            asset_code: "USD".to_string(),
        };

        assert!(validate_payload(&payload).is_ok());
    }

    #[test]
    fn test_validate_payload_zero_amount() {
        let payload = CallbackPayload {
            id: "anchor-123".to_string(),
            amount_in: "0".to_string(),
            stellar_account: "GABCDEFGHIJKLMNOPQRSTUVWXYZ234567890ABCDEFGHIJKLMNOPQR".to_string(),
            asset_code: "USD".to_string(),
        };

        assert!(validate_payload(&payload).is_err());
    }

    #[test]
    fn test_validate_payload_negative_amount() {
        let payload = CallbackPayload {
            id: "anchor-123".to_string(),
            amount_in: "-10".to_string(),
            stellar_account: "GABCDEFGHIJKLMNOPQRSTUVWXYZ234567890ABCDEFGHIJKLMNOPQR".to_string(),
            asset_code: "USD".to_string(),
        };

        assert!(validate_payload(&payload).is_err());
    }

    #[test]
    fn test_validate_payload_invalid_stellar_account_length() {
        let payload = CallbackPayload {
            id: "anchor-123".to_string(),
            amount_in: "100".to_string(),
            stellar_account: "GABCD".to_string(),
            asset_code: "USD".to_string(),
        };

        assert!(validate_payload(&payload).is_err());
    }

    #[test]
    fn test_validate_payload_invalid_stellar_account_prefix() {
        let payload = CallbackPayload {
            id: "anchor-123".to_string(),
            amount_in: "100".to_string(),
            stellar_account: "XABCDEFGHIJKLMNOPQRSTUVWXYZ234567890ABCDEFGHIJKLMNOPQR".to_string(),
            asset_code: "USD".to_string(),
        };

        assert!(validate_payload(&payload).is_err());
    }

    #[test]
    fn test_validate_payload_empty_asset_code() {
        let payload = CallbackPayload {
            id: "anchor-123".to_string(),
            amount_in: "100".to_string(),
            stellar_account: "GABCDEFGHIJKLMNOPQRSTUVWXYZ234567890ABCDEFGHIJKLMNOPQR".to_string(),
            asset_code: "".to_string(),
        };

        assert!(validate_payload(&payload).is_err());
    }

    #[test]
    fn test_validate_payload_asset_code_too_long() {
        let payload = CallbackPayload {
            id: "anchor-123".to_string(),
            amount_in: "100".to_string(),
            stellar_account: "GABCDEFGHIJKLMNOPQRSTUVWXYZ234567890ABCDEFGHIJKLMNOPQR".to_string(),
            asset_code: "VERYLONGASSETCODE".to_string(),
        };

        assert!(validate_payload(&payload).is_err());
    }
}
