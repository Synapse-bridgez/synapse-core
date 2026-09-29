use crate::db::{models::Transaction, queries};
use crate::error::AppError;
use crate::utils::cursor as cursor_util;
use crate::validation::{
    sanitize_string, validate_asset_code, validate_max_len, validate_positive_amount,
    validate_stellar_address,
};
use crate::{ApiState, AppState};
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use csv::Reader;
use serde::{Deserialize, Serialize};
use sqlx::types::BigDecimal;
use std::str::FromStr;
use tracing::instrument;
use uuid::Uuid;

const MAX_IMPORT_ROWS: usize = 10_000;
const MAX_IMPORT_BYTES: usize = 10 * 1024 * 1024; // 10MB

#[derive(Debug, Deserialize)]
pub struct ImportCsvRow {
    pub stellar_account: String,
    pub amount: String,
    pub asset_code: String,
    pub status: Option<String>,
    pub anchor_transaction_id: Option<String>,
    pub callback_type: Option<String>,
    pub callback_status: Option<String>,
    pub memo: Option<String>,
    pub memo_type: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ImportRowResult {
    pub row_number: usize,
    pub success: bool,
    pub transaction_id: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ImportResponse {
    pub total_rows: usize,
    pub accepted: usize,
    pub rejected: usize,
    pub errors: Vec<ImportRowResult>,
}

struct ValidatedTransaction {
    stellar_account: String,
    amount: BigDecimal,
    asset_code: String,
    status: String,
    anchor_transaction_id: Option<String>,
    callback_type: Option<String>,
    callback_status: Option<String>,
    memo: Option<String>,
    memo_type: Option<String>,
}

fn validate_import_row(row: ImportCsvRow, row_number: usize) -> Result<ValidatedTransaction, String> {
    // Validate stellar account
    validate_stellar_address(&row.stellar_account)
        .map_err(|e| format!("Row {}: Invalid stellar_account - {}", row_number, e))?;

    // Validate amount
    let amount = BigDecimal::from_str(&row.amount)
        .map_err(|_| format!("Row {}: Invalid amount format", row_number))?;
    validate_positive_amount(&amount)
        .map_err(|e| format!("Row {}: {}", row_number, e))?;

    // Validate asset code
    validate_asset_code(&row.asset_code)
        .map_err(|e| format!("Row {}: Invalid asset_code - {}", row_number, e))?;

    // Validate status if provided
    let status = match &row.status {
        Some(s) => {
            let s = s.to_lowercase();
            match s.as_str() {
                "pending" | "processing" | "completed" | "failed" => s,
                _ => return Err(format!(
                    "Row {}: Invalid status '{}'. Must be one of: pending, processing, completed, failed",
                    row_number, s
                )),
            }
        }
        None => "pending".to_string(),
    };

    // Validate optional fields
    let anchor_tx_id = row.anchor_transaction_id
        .map(|s| sanitize_string(&s))
        .filter(|s| !s.is_empty());

    let callback_type = row.callback_type
        .map(|s| sanitize_string(&s))
        .filter(|s| !s.is_empty());

    let callback_status = row.callback_status
        .map(|s| sanitize_string(&s))
        .filter(|s| !s.is_empty());

    let memo = row.memo
        .map(|s| sanitize_string(&s))
        .filter(|s| !s.is_empty());

    // Validate memo_type if provided
    if let Some(mt) = &row.memo_type {
        match mt.as_str() {
            "text" | "hash" | "id" => {}
            _ => return Err(format!(
                "Row {}: Invalid memo_type '{}'. Must be one of: text, hash, id",
                row_number, mt
            )),
        }
    }

    Ok(ValidatedTransaction {
        stellar_account: row.stellar_account,
        amount,
        asset_code: row.asset_code,
        status,
        anchor_transaction_id: anchor_tx_id,
        callback_type,
        callback_status,
        memo,
        memo_type: row.memo_type,
    })
}

/// Import transactions from CSV file
///
/// Accepts a CSV file with transaction data. Each row is validated and imported
/// with audit logging. All rows are processed independently; failures in one row
/// do not affect others.
///
/// # CSV Schema
/// Required columns: stellar_account, amount, asset_code
/// Optional columns: status, anchor_transaction_id, callback_type, callback_status, memo, memo_type
///
/// # Errors
/// - `400 Bad Request` – malformed CSV or validation errors
/// - `413 Payload Too Large` – file exceeds size limit
/// - `500 Internal Server Error` – database error
#[instrument(name = "import.csv_upload", skip(state, body))]
pub async fn import_csv_upload(
    State(state): State<ApiState>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    crate::metrics::admin_transaction_import_requests_total()
        .add(1, &[opentelemetry::KeyValue::new("operation", "csv_import")]);

    // Check file size
    if body.len() > MAX_IMPORT_BYTES {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({
                "error": format!("File exceeds maximum size of {} bytes", MAX_IMPORT_BYTES)
            })),
        )
            .into_response();
    }

    // Parse CSV
    let mut reader = Reader::from_reader(&body[..]);
    let mut total_rows = 0;
    let mut accepted = 0;
    let mut rejected = 0;
    let mut errors = Vec::new();

    for (row_idx, result) in reader.deserialize().enumerate() {
        let row_number = row_idx + 2; // +2 because +1 for header, +1 for 1-based indexing

        if total_rows >= MAX_IMPORT_ROWS {
            errors.push(ImportRowResult {
                row_number,
                success: false,
                transaction_id: None,
                error: Some(format!(
                    "Import limit of {} rows exceeded",
                    MAX_IMPORT_ROWS
                )),
            });
            rejected += 1;
            break;
        }

        total_rows += 1;

        let row: Result<ImportCsvRow, _> = result;
        let import_result = match row {
            Err(e) => ImportRowResult {
                row_number,
                success: false,
                transaction_id: None,
                error: Some(format!("CSV parse error: {}", e)),
            },
            Ok(row) => {
                match validate_import_row(row, row_number) {
                    Err(e) => ImportRowResult {
                        row_number,
                        success: false,
                        transaction_id: None,
                        error: Some(e),
                    },
                    Ok(validated) => {
                        // Create transaction
                        let tx = Transaction::new(
                            validated.stellar_account,
                            validated.amount,
                            validated.asset_code,
                            validated.anchor_transaction_id,
                            validated.callback_type,
                            validated.callback_status,
                            validated.memo,
                            validated.memo_type,
                            None,
                        );

                        // Try to insert
                        match queries::insert_transaction(
                            &state.app_state.db,
                            &tx,
                            Some(&state.app_state.query_cache),
                        )
                        .await
                        {
                            Ok(_) => {
                                // Log audit event for imported transaction
                                if let Err(e) = log_import_audit(
                                    &state.app_state.db,
                                    tx.id,
                                    &tx.stellar_account,
                                )
                                .await
                                {
                                    tracing::error!("Failed to log import audit: {}", e);
                                }

                                accepted += 1;
                                ImportRowResult {
                                    row_number,
                                    success: true,
                                    transaction_id: Some(tx.id.to_string()),
                                    error: None,
                                }
                            }
                            Err(e) => {
                                rejected += 1;
                                ImportRowResult {
                                    row_number,
                                    success: false,
                                    transaction_id: None,
                                    error: Some(format!("Database error: {}", e)),
                                }
                            }
                        }
                    }
                }
            }
        };

        // Only include errors in response (to keep response smaller)
        if !import_result.success {
            rejected += 1;
            errors.push(import_result);
        } else {
            accepted += 1;
        }
    }

    let response = ImportResponse {
        total_rows,
        accepted,
        rejected,
        errors,
    };

    (StatusCode::OK, Json(response)).into_response()
}

async fn log_import_audit(
    pool: &sqlx::PgPool,
    transaction_id: Uuid,
    stellar_account: &str,
) -> Result<(), AppError> {
    // Log to audit table
    sqlx::query(
        "INSERT INTO audit_logs (action, resource_type, resource_id, details, created_at)
         VALUES ('transaction_imported', 'transaction', $1, $2, NOW())",
    )
    .bind(transaction_id.to_string())
    .bind(serde_json::json!({
        "stellar_account": stellar_account,
        "import_type": "csv_batch"
    }))
    .execute(pool)
    .await
    .map_err(|e| AppError::DatabaseError(e.to_string()))?;

    tracing::info!(
        transaction_id = %transaction_id,
        stellar_account = stellar_account,
        "Transaction imported from CSV"
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_import_row_valid() {
        let row = ImportCsvRow {
            stellar_account: "GBBD47UZQ2EORUNPA47QO476PHFBHVA5H46LSR5BTNRK4A4KM4A6TB".to_string(),
            amount: "100.50".to_string(),
            asset_code: "USD".to_string(),
            status: Some("pending".to_string()),
            anchor_transaction_id: None,
            callback_type: None,
            callback_status: None,
            memo: None,
            memo_type: None,
        };

        assert!(validate_import_row(row, 1).is_ok());
    }

    #[test]
    fn test_validate_import_row_invalid_stellar_account() {
        let row = ImportCsvRow {
            stellar_account: "invalid".to_string(),
            amount: "100.50".to_string(),
            asset_code: "USD".to_string(),
            status: None,
            anchor_transaction_id: None,
            callback_type: None,
            callback_status: None,
            memo: None,
            memo_type: None,
        };

        assert!(validate_import_row(row, 1).is_err());
    }

    #[test]
    fn test_validate_import_row_invalid_amount() {
        let row = ImportCsvRow {
            stellar_account: "GBBD47UZQ2EORUNPA47QO476PHFBHVA5H46LSR5BTNRK4A4KM4A6TB".to_string(),
            amount: "invalid".to_string(),
            asset_code: "USD".to_string(),
            status: None,
            anchor_transaction_id: None,
            callback_type: None,
            callback_status: None,
            memo: None,
            memo_type: None,
        };

        assert!(validate_import_row(row, 1).is_err());
    }

    #[test]
    fn test_validate_import_row_negative_amount() {
        let row = ImportCsvRow {
            stellar_account: "GBBD47UZQ2EORUNPA47QO476PHFBHVA5H46LSR5BTNRK4A4KM4A6TB".to_string(),
            amount: "-100.50".to_string(),
            asset_code: "USD".to_string(),
            status: None,
            anchor_transaction_id: None,
            callback_type: None,
            callback_status: None,
            memo: None,
            memo_type: None,
        };

        assert!(validate_import_row(row, 1).is_err());
    }
}
