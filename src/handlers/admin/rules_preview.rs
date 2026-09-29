//! Asset processing rules engine dry-run/preview mode (Issue #1260)
//!
//! Provides a preview endpoint enabling operators to test rule changes against
//! real transaction data without executing side effects such as webhook dispatch
//! or database writes.

use crate::error::AppError;
use crate::db::models::Transaction;
use crate::AppState;
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use uuid::Uuid;
use sqlx::Row;

#[derive(Debug, Deserialize)]
pub struct RulesPreviewRequest {
    /// Rule definition to test (JSON object with filter criteria)
    pub rule: JsonValue,
    /// Either transaction_id (to fetch from DB) or raw transaction data
    #[serde(flatten)]
    pub transaction_input: TransactionInput,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum TransactionInput {
    ById {
        transaction_id: Uuid,
    },
    Inline {
        transaction_data: TransactionData,
    },
}

#[derive(Debug, Deserialize)]
pub struct TransactionData {
    pub id: Option<String>,
    pub amount: String,
    pub asset_code: String,
    pub event_type: Option<String>,
    pub memo: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RulesPreviewResponse {
    pub matched: bool,
    pub rule: JsonValue,
    pub transaction: TransactionPreviewData,
    pub matched_criteria: Vec<String>,
    pub unmatched_criteria: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct TransactionPreviewData {
    pub id: String,
    pub amount: String,
    pub asset_code: String,
    pub event_type: Option<String>,
    pub memo: Option<String>,
}

/// Evaluate rules against transaction data in dry-run mode
/// POST /admin/rules/preview
pub async fn preview_rules(
    State(state): State<AppState>,
    Json(req): Json<RulesPreviewRequest>,
) -> Result<impl IntoResponse, AppError> {
    // Validate rule format
    if !req.rule.is_object() {
        return Err(AppError::BadRequest(
            "rule must be a JSON object".to_string(),
        ));
    }

    // Load transaction data
    let tx_data = match req.transaction_input {
        TransactionInput::ById { transaction_id } => {
            // Fetch from database
            let row = sqlx::query(
                "SELECT id, amount, asset_code, (payload->>'event_type') as event_type, memo FROM transactions WHERE id = $1"
            )
            .bind(transaction_id)
            .fetch_optional(&state.db_pool)
            .await
            .map_err(|e| {
                tracing::error!("Failed to fetch transaction for preview: {}", e);
                AppError::InternalServerError("Failed to fetch transaction".to_string())
            })?
            .ok_or_else(|| AppError::NotFound("Transaction not found".to_string()))?;

            TransactionData {
                id: Some(row.get::<String, _>("id")),
                amount: row.get("amount"),
                asset_code: row.get("asset_code"),
                event_type: row.get("event_type"),
                memo: row.get("memo"),
            }
        }
        TransactionInput::Inline { transaction_data } => transaction_data,
    };

    // Evaluate rules
    let (matched, matched_criteria, unmatched_criteria) = evaluate_rules(&req.rule, &tx_data)?;

    let preview_data = TransactionPreviewData {
        id: tx_data.id.unwrap_or_else(|| "unknown".to_string()),
        amount: tx_data.amount.clone(),
        asset_code: tx_data.asset_code.clone(),
        event_type: tx_data.event_type.clone(),
        memo: tx_data.memo.clone(),
    };

    Ok((
        StatusCode::OK,
        Json(RulesPreviewResponse {
            matched,
            rule: req.rule,
            transaction: preview_data,
            matched_criteria,
            unmatched_criteria,
        }),
    ))
}

/// Evaluate rules against transaction data
/// Returns (matched, matched_criteria, unmatched_criteria)
fn evaluate_rules(
    rule: &JsonValue,
    tx_data: &TransactionData,
) -> Result<(bool, Vec<String>, Vec<String>), AppError> {
    let mut matched_criteria = Vec::new();
    let mut unmatched_criteria = Vec::new();

    // Evaluate asset_codes filter
    if let Some(codes) = rule.get("asset_codes") {
        let codes_array = codes.as_array().ok_or_else(|| {
            AppError::BadRequest("asset_codes must be an array".to_string())
        })?;

        let asset_codes: Vec<String> = codes_array
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();

        if asset_codes.contains(&tx_data.asset_code) {
            matched_criteria.push(format!("asset_code matches {:?}", asset_codes));
        } else {
            unmatched_criteria.push(format!(
                "asset_code '{}' not in {:?}",
                tx_data.asset_code, asset_codes
            ));
        }
    }

    // Evaluate min_amount filter
    if let Some(min_str) = rule.get("min_amount") {
        let min_val: f64 = min_str
            .as_str()
            .ok_or_else(|| AppError::BadRequest("min_amount must be a string".to_string()))?
            .parse()
            .map_err(|_| AppError::BadRequest("min_amount is not a valid decimal".to_string()))?;

        let tx_amount: f64 = tx_data
            .amount
            .parse()
            .map_err(|_| AppError::BadRequest("transaction amount is not a valid decimal".to_string()))?;

        if tx_amount >= min_val {
            matched_criteria.push(format!("amount {} >= min_amount {}", tx_amount, min_val));
        } else {
            unmatched_criteria.push(format!(
                "amount {} < min_amount {}",
                tx_amount, min_val
            ));
        }
    }

    // Evaluate max_amount filter
    if let Some(max_str) = rule.get("max_amount") {
        let max_val: f64 = max_str
            .as_str()
            .ok_or_else(|| AppError::BadRequest("max_amount must be a string".to_string()))?
            .parse()
            .map_err(|_| AppError::BadRequest("max_amount is not a valid decimal".to_string()))?;

        let tx_amount: f64 = tx_data
            .amount
            .parse()
            .map_err(|_| AppError::BadRequest("transaction amount is not a valid decimal".to_string()))?;

        if tx_amount <= max_val {
            matched_criteria.push(format!("amount {} <= max_amount {}", tx_amount, max_val));
        } else {
            unmatched_criteria.push(format!(
                "amount {} > max_amount {}",
                tx_amount, max_val
            ));
        }
    }

    // Evaluate event_types filter
    if let Some(types) = rule.get("event_types") {
        let types_array = types.as_array().ok_or_else(|| {
            AppError::BadRequest("event_types must be an array".to_string())
        })?;

        let event_types: Vec<String> = types_array
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();

        if let Some(tx_event) = &tx_data.event_type {
            if event_types.contains(tx_event) {
                matched_criteria.push(format!("event_type matches {:?}", event_types));
            } else {
                unmatched_criteria.push(format!(
                    "event_type '{}' not in {:?}",
                    tx_event, event_types
                ));
            }
        } else {
            unmatched_criteria.push("event_type not provided".to_string());
        }
    }

    // All criteria must match (AND logic)
    let overall_matched = unmatched_criteria.is_empty();

    Ok((overall_matched, matched_criteria, unmatched_criteria))
}
