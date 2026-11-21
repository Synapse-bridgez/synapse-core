use crate::error::AppError;
use axum::{
    extract::{Query, State},
    http::{header, header::HeaderValue, HeaderMap, StatusCode},
    response::IntoResponse,
};
use chrono::{DateTime, Utc};
use csv::Writer;
use futures::stream::{Stream, StreamExt};
use serde::Deserialize;
use serde::Serialize;
use sqlx::{PgPool, Row};
use std::pin::Pin;
use std::sync::Arc;

use crate::db::models::Transaction;

/// Query parameters for the export endpoint
#[derive(Debug, Deserialize, Clone)]
pub struct ExportQuery {
    /// Export format: "csv" or "json"
    #[serde(default = "default_format")]
    pub format: String,
    /// Start date filter (inclusive) - format: YYYY-MM-DD
    pub from: Option<String>,
    /// End date filter (inclusive) - format: YYYY-MM-DD
    pub to: Option<String>,
    /// Filter by transaction status
    pub status: Option<String>,
    /// Filter by asset code
    pub asset_code: Option<String>,
}

fn default_format() -> String {
    "csv".to_string()
}

impl Default for ExportQuery {
    fn default() -> Self {
        Self {
            format: default_format(),
            from: None,
            to: None,
            status: None,
            asset_code: None,
        }
    }
}

impl ExportQuery {
    /// Validate the export query parameters before running the export.
    pub fn validate(&self) -> Result<(), AppError> {
        let format = self.format.to_lowercase();
        if format != "csv" && format != "json" {
            return Err(AppError::Validation(
                "Export format must be either 'csv' or 'json'".to_string(),
            ));
        }

        if let Some(ref from) = self.from {
            parse_date(from).map(|_| ()).map_err(AppError::Validation)?;
        }

        if let Some(ref to) = self.to {
            parse_date(to).map(|_| ()).map_err(AppError::Validation)?;
        }

        Ok(())
    }
}

/// CSV row representation - uses String for amount to avoid Serialize issues with BigDecimal
#[derive(Serialize)]
struct TransactionCsvRow {
    id: String,
    stellar_account: String,
    amount: String,
    asset_code: String,
    status: String,
    created_at: String,
    updated_at: String,
    anchor_transaction_id: String,
    callback_type: String,
    callback_status: String,
}

/// JSON representation for export - converts BigDecimal to String
#[derive(Serialize)]
struct TransactionJsonRow {
    id: String,
    stellar_account: String,
    amount: String,
    asset_code: String,
    status: String,
    created_at: String,
    updated_at: String,
    anchor_transaction_id: Option<String>,
    callback_type: Option<String>,
    callback_status: Option<String>,
}

impl From<&Transaction> for TransactionCsvRow {
    fn from(tx: &Transaction) -> Self {
        TransactionCsvRow {
            id: tx.id.to_string(),
            stellar_account: tx.stellar_account.clone(),
            amount: tx.amount.to_string(),
            asset_code: tx.asset_code.clone(),
            status: tx.status.clone(),
            created_at: tx.created_at.to_rfc3339(),
            updated_at: tx.updated_at.to_rfc3339(),
            anchor_transaction_id: tx.anchor_transaction_id.clone().unwrap_or_default(),
            callback_type: tx.callback_type.clone().unwrap_or_default(),
            callback_status: tx.callback_status.clone().unwrap_or_default(),
        }
    }
}

impl From<&Transaction> for TransactionJsonRow {
    fn from(tx: &Transaction) -> Self {
        TransactionJsonRow {
            id: tx.id.to_string(),
            stellar_account: tx.stellar_account.clone(),
            amount: tx.amount.to_string(),
            asset_code: tx.asset_code.clone(),
            status: tx.status.clone(),
            created_at: tx.created_at.to_rfc3339(),
            updated_at: tx.updated_at.to_rfc3339(),
            anchor_transaction_id: tx.anchor_transaction_id.clone(),
            callback_type: tx.callback_type.clone(),
            callback_status: tx.callback_status.clone(),
        }
    }
}

/// Batch size for cursor-based streaming
const BATCH_SIZE: i64 = 1000;

/// Type alias for the stream of CSV rows
///
/// Each item is a chunk of the export body. Errors are surfaced as
/// `AppError` so the response can be terminated with a clear failure
/// instead of a silently truncated success.
type CsvStream = Pin<Box<dyn Stream<Item = Result<String, AppError>> + Send>>;

/// Type alias for the stream of JSON rows
type JsonStream = Pin<Box<dyn Stream<Item = Result<String, AppError>> + Send>>;

/// Parse date string to DateTime<Utc>
fn parse_date(date_str: &str) -> Result<DateTime<Utc>, String> {
    // Handle both YYYY-MM-DD and YYYY-MM-DDTHH:MM:SSZ formats
    let date_str = if date_str.len() == 10 {
        format!("{date_str}T00:00:00Z")
    } else {
        date_str.to_string()
    };

    DateTime::parse_from_rfc3339(&date_str)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| format!("Invalid date format: {e}"))
}

/// Build SQL filter conditions based on query parameters
fn build_filter_conditions(
    from: &Option<String>,
    to: &Option<String>,
    status: &Option<String>,
    asset_code: &Option<String>,
) -> (String, Vec<FilterValue>) {
    let mut conditions = Vec::new();
    let mut params = Vec::new();
    let mut param_count = 1;

    if let Some(ref from_date) = from {
        if let Ok(parsed) = parse_date(from_date) {
            conditions.push(format!("created_at >= ${param_count}"));
            params.push(FilterValue::DateTime(parsed));
            param_count += 1;
        }
    }

    if let Some(ref to_date) = to {
        if let Ok(parsed) = parse_date(to_date) {
            // Add one day to include the entire end date
            let end_of_day = parsed + chrono::Duration::days(1);
            conditions.push(format!("created_at < ${param_count}"));
            params.push(FilterValue::DateTime(end_of_day));
            param_count += 1;
        }
    }

    if let Some(ref status_val) = status {
        conditions.push(format!("status = ${param_count}"));
        params.push(FilterValue::String(status_val.clone()));
        param_count += 1;
    }

    if let Some(ref asset) = asset_code {
        conditions.push(format!("asset_code = ${param_count}"));
        params.push(FilterValue::String(asset.clone()));
    }

    let where_clause = if conditions.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conditions.join(" AND "))
    };

    (where_clause, params)
}

/// Filter value enum for dynamic parameter handling
enum FilterValue {
    String(String),
    DateTime(DateTime<Utc>),
}

/// Create a CSV stream from database rows - truly streaming without buffering
fn create_csv_stream(
    pool: Arc<PgPool>,
    from: Option<String>,
    to: Option<String>,
    status: Option<String>,
    asset_code: Option<String>,
) -> CsvStream {
    let pool_clone = pool.clone();

    Box::pin(async_stream::stream! {
        let mut last_id: Option<uuid::Uuid> = None;

        // First, write CSV header
        let headers = "id,stellar_account,amount,asset_code,status,created_at,updated_at,anchor_transaction_id,callback_type,callback_status";
        yield Ok(headers.to_string());

        loop {
            // Build base query with filters
            let (where_clause, params) = build_filter_conditions(&from, &to, &status, &asset_code);

            let mut sql = format!(
                "SELECT id, stellar_account, amount, asset_code, status, created_at, updated_at,
                        anchor_transaction_id, callback_type, callback_status, settlement_id,
                        memo, memo_type, metadata
                 FROM transactions {where_clause}"
            );

            // Add cursor and limit
            if let Some(id) = last_id {
                if where_clause.is_empty() {
                    sql = format!("{sql} WHERE id > '{id}' ORDER BY id ASC LIMIT {BATCH_SIZE}");
                } else {
                    sql = format!("{sql} AND id > '{id}' ORDER BY id ASC LIMIT {BATCH_SIZE}");
                }
            } else {
                sql = format!("{sql} ORDER BY id ASC LIMIT {BATCH_SIZE}");
            }

            let mut query = sqlx::query(&sql);
            for param in &params {
                query = match param {
                    FilterValue::String(s) => query.bind(s),
                    FilterValue::DateTime(dt) => query.bind(dt),
                };
            }

            let rows = match query.fetch_all(pool_clone.as_ref()).await {
                Ok(rows) => rows,
                Err(e) => {
                    yield Err(AppError::Database(e));
                    return;
                }
            };

            if rows.is_empty() {
                break;
            }

            let mut batch = String::new();
            let mut last_row_id: Option<uuid::Uuid> = None;

            for row in &rows {
                let tx = match Transaction::from_row(row) {
                    Ok(tx) => tx,
                    Err(e) => {
                        yield Err(AppError::Database(e));
                        return;
                    }
                };
                last_row_id = Some(tx.id);

                let csv_row = TransactionCsvRow::from(&tx);
                let mut wtr = Writer::from_writer(vec![]);
                if let Err(e) = wtr.serialize(&csv_row) {
                    yield Err(AppError::Internal(format!("CSV serialization failed: {e}")));
                    return;
                }
                let data = match wtr.into_inner() {
                    Ok(data) => data,
                    Err(e) => {
                        yield Err(AppError::Internal(format!("CSV writer failed: {e}")));
                        return;
                    }
                };
                match String::from_utf8(data) {
                    Ok(s) => batch.push_str(&s),
                    Err(e) => {
                        yield Err(AppError::Internal(format!("CSV encoding failed: {e}")));
                        return;
                    }
                }
            }

            // Emit the batch incrementally so memory stays bounded by BATCH_SIZE.
            yield Ok(batch);

            last_id = last_row_id;

            if (rows.len() as i64) < BATCH_SIZE {
                break;
            }
        }
    })
}

/// Create a JSON stream from database rows - truly streaming without buffering
fn create_json_stream(
    pool: Arc<PgPool>,
    from: Option<String>,
    to: Option<String>,
    status: Option<String>,
    asset_code: Option<String>,
) -> JsonStream {
    let pool_clone = pool.clone();

    Box::pin(async_stream::stream! {
        let mut last_id: Option<uuid::Uuid> = None;
        let mut first = true;

        // Open the JSON array
        yield Ok("[".to_string());

        loop {
            let (where_clause, params) = build_filter_conditions(&from, &to, &status, &asset_code);

            let mut sql = format!(
                "SELECT id, stellar_account, amount, asset_code, status, created_at, updated_at,
                        anchor_transaction_id, callback_type, callback_status, settlement_id,
                        memo, memo_type, metadata
                 FROM transactions {where_clause}"
            );

            if let Some(id) = last_id {
                if where_clause.is_empty() {
                    sql = format!("{sql} WHERE id > '{id}' ORDER BY id ASC LIMIT {BATCH_SIZE}");
                } else {
                    sql = format!("{sql} AND id > '{id}' ORDER BY id ASC LIMIT {BATCH_SIZE}");
                }
            } else {
                sql = format!("{sql} ORDER BY id ASC LIMIT {BATCH_SIZE}");
            }

            let mut query = sqlx::query(&sql);
            for param in &params {
                query = match param {
                    FilterValue::String(s) => query.bind(s),
                    FilterValue::DateTime(dt) => query.bind(dt),
                };
            }

            let rows = match query.fetch_all(pool_clone.as_ref()).await {
                Ok(rows) => rows,
                Err(e) => {
                    yield Err(AppError::Database(e));
                    return;
                }
            };

            if rows.is_empty() {
                break;
            }

            let mut batch = String::new();
            let mut last_row_id: Option<uuid::Uuid> = None;

            for row in &rows {
                let tx = match Transaction::from_row(row) {
                    Ok(tx) => tx,
                    Err(e) => {
                        yield Err(AppError::Database(e));
                        return;
                    }
                };
                last_row_id = Some(tx.id);

                let json_row = TransactionJsonRow::from(&tx);
                let serialized = match serde_json::to_string(&json_row) {
                    Ok(s) => s,
                    Err(e) => {
                        yield Err(AppError::Internal(format!("JSON serialization failed: {e}")));
                        return;
                    }
                };

                if !first {
                    batch.push(',');
                }
                first = false;
                batch.push_str(&serialized);
            }

            yield Ok(batch);

            last_id = last_row_id;

            if (rows.len() as i64) < BATCH_SIZE {
                break;
            }
        }

        // Close the JSON array
        yield Ok("]".to_string());
    })
}

/// Export transactions as CSV or JSON, streaming rows to the client.
///
/// Rows are fetched in cursor-based batches and written incrementally so
/// memory use does not scale with the total export size. If an error occurs
/// mid-stream the response body is terminated with an error marker rather
/// than silently truncating a success response.
pub async fn export_transactions(
    State(pool): State<Arc<PgPool>>,
    Query(query): Query<ExportQuery>,
) -> Result<impl IntoResponse, AppError> {
    query.validate()?;

    let format = query.format.to_lowercase();

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(if format == "csv" {
            "text/csv; charset=utf-8"
        } else {
            "application/json; charset=utf-8"
        }),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static(if format == "csv" {
            "attachment; filename=\"transactions.csv\""
        } else {
            "attachment; filename=\"transactions.json\""
        }),
    );

    let stream: Pin<Box<dyn Stream<Item = Result<String, AppError>> + Send>> = if format == "csv" {
        create_csv_stream(
            pool,
            query.from.clone(),
            query.to.clone(),
            query.status.clone(),
            query.asset_code.clone(),
        )
    } else {
        create_json_stream(
            pool,
            query.from.clone(),
            query.to.clone(),
            query.status.clone(),
            query.asset_code.clone(),
        )
    };

    // Map stream errors into a clear failure marker appended to the body so
    // clients can distinguish a truncated success from a real failure.
    let body_stream = stream.map(|item| match item {
        Ok(chunk) => Ok::<_, std::io::Error>(axum::body::Bytes::from(chunk)),
        Err(e) => {
            tracing::error!(error = %e, "export stream failed mid-flight");
            Ok(axum::body::Bytes::from(format!("\n# export failed: {e}\n")))
        }
    });

    Ok((StatusCode::OK, headers, axum::body::Body::from_stream(body_stream)))
}
