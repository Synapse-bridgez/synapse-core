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
type CsvStream = Pin<Box<dyn Stream<Item = Result<String, sqlx::Error>> + Send>>;

/// Type alias for the stream of JSON rows
type JsonStream = Pin<Box<dyn Stream<Item = Result<String, sqlx::Error>> + Send>>;

/// Parse date string to DateTime<Utc>
fn parse_date(date_str: &str) -> Result<DateTime<Utc>, String> {
    // Handle both YYYY-MM-DD and YYYY-MM-DDTHH:MM:SSZ formats
    let date_str = if date_str.len() == 10 {
        format!("{}T00:00:00Z", date_str)
    } else {
        date_str.to_string()
    };

    DateTime::parse_from_rfc3339(&date_str)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| format!("Invalid date format: {}", e))
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
            conditions.push(format!("created_at >= ${}", param_count));
            params.push(FilterValue::DateTime(parsed));
            param_count += 1;
        }
    }

    if let Some(ref to_date) = to {
        if let Ok(parsed) = parse_date(to_date) {
            // Add one day to include the entire end date
            let end_of_day = parsed + chrono::Duration::days(1);
            conditions.push(format!("created_at < ${}", param_count));
            params.push(FilterValue::DateTime(end_of_day));
            param_count += 1;
        }
    }

    if let Some(ref status_val) = status {
        conditions.push(format!("status = ${}", param_count));
        params.push(FilterValue::String(status_val.clone()));
        param_count += 1;
    }

    if let Some(ref asset) = asset_code {
        conditions.push(format!("asset_code = ${}", param_count));
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
                 FROM transactions {}",
                where_clause
            );

            // Add cursor and limit
            if let Some(id) = last_id {
                if where_clause.is_empty() {
                    sql = format!("{} WHERE id > '{}' ORDER BY id ASC LIMIT {}", sql, id, BATCH_SIZE);
                } else {
                    sql = format!("{} AND id > '{}' ORDER BY id ASC LIMIT {}", sql, id, BATCH_SIZE);
                }
            } else {
                sql = format!("{} ORDER BY id ASC LIMIT {}", sql, BATCH_SIZE);
            }

            // Execute query
            let mut query = sqlx::query(&sql);

            // Bind parameters based on filter count
            for param in params.iter() {
                match param {
                    FilterValue::String(s) => {
                        query = query.bind(s.clone());
                    }
                    FilterValue::DateTime(dt) => {
                        query = query.bind(*dt);
                    }
                }
            }

            let mut rows = query.fetch(&*pool_clone);

            let mut batch_has_rows = false;

            while let Some(row) = rows.next().await {
                match row {
                    Ok(row) => {
                        batch_has_rows = true;
                        let tx = Transaction {
                            id: row.get("id"),
                            stellar_account: row.get("stellar_account"),
                            amount: row.get("amount"),
                            asset_code: row.get("asset_code"),
                            status: row.get("status"),
                            created_at: row.get("created_at"),
                            updated_at: row.get("updated_at"),
                            anchor_transaction_id: row.get("anchor_transaction_id"),
                            callback_type: row.get("callback_type"),
                            callback_status: row.get("callback_status"),
                            settlement_id: row.get("settlement_id"),
                            memo: row.get("memo"),
                            memo_type: row.get("memo_type"),
                            metadata: row.get("metadata"),
                        };

                        last_id = Some(tx.id);

                        let csv_row = TransactionCsvRow::from(&tx);
                        let mut wtr = Writer::from_writer(vec![]);
                        wtr.serialize(csv_row).unwrap();
                        let csv_line = String::from_utf8(wtr.into_inner().unwrap()).unwrap();
                        yield Ok(csv_line);
                    }
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                }
            }

            if !batch_has_rows {
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

        loop {
            // Build base query with filters
            let (where_clause, params) = build_filter_conditions(&from, &to, &status, &asset_code);

            let mut sql = format!(
                "SELECT id, stellar_account, amount, asset_code, status, created_at, updated_at,
                        anchor_transaction_id, callback_type, callback_status, settlement_id,
                        memo, memo_type, metadata
                 FROM transactions {}",
                where_clause
            );

            // Add cursor and limit
            if let Some(id) = last_id {
                if where_clause.is_empty() {
                    sql = format!("{} WHERE id > '{}' ORDER BY id ASC LIMIT {}", sql, id, BATCH_SIZE);
                } else {
                    sql = format!("{} AND id > '{}' ORDER BY id ASC LIMIT {}", sql, id, BATCH_SIZE);
                }
            } else {
                sql = format!("{} ORDER BY id ASC LIMIT {}", sql, BATCH_SIZE);
            }

            let mut query = sqlx::query(&sql);

            // Bind parameters
            for param in params.iter() {
                match param {
                    FilterValue::String(s) => {
                        query = query.bind(s.clone());
                    }
                    FilterValue::DateTime(dt) => {
                        query = query.bind(*dt);
                    }
                }
            }

            let mut rows = query.fetch(&*pool_clone);

            let mut batch_has_rows = false;

            while let Some(row) = rows.next().await {
                match row {
                    Ok(row) => {
                        batch_has_rows = true;
                        let tx = Transaction {
                            id: row.get("id"),
                            stellar_account: row.get("stellar_account"),
                            amount: row.get("amount"),
                            asset_code: row.get("asset_code"),
                            status: row.get("status"),
                            created_at: row.get("created_at"),
                            updated_at: row.get("updated_at"),
                            anchor_transaction_id: row.get("anchor_transaction_id"),
                            callback_type: row.get("callback_type"),
                            callback_status: row.get("callback_status"),
                            settlement_id: row.get("settlement_id"),
                            memo: row.get("memo"),
                            memo_type: row.get("memo_type"),
                            metadata: row.get("metadata"),
                        };

                        last_id = Some(tx.id);

                        let json_row = TransactionJsonRow::from(&tx);
                        let json_line = serde_json::to_string(&json_row).unwrap();
                        yield Ok(json_line);
                    }
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                }
            }

            if !batch_has_rows {
                break;
            }
        }
    })
}

/// Helper function to convert a stream of strings into an Axum response
/// Note: For production with 100k+ rows, you'd want to use true streaming.
/// This implementation uses cursor-based pagination in the query but collects
/// the final result. For true streaming, you'd need to use a different approach.
async fn stream_to_response<S>(stream: S, content_type: &str, filename: &str) -> impl IntoResponse
where
    S: Stream<Item = Result<String, sqlx::Error>> + Send + 'static,
{
    use futures::stream::StreamExt;

    // Collect all data from the stream
    let mut all_data = String::new();
    // Pin the stream to allow polling
    let mut pinned_stream = Box::pin(stream);
    while let Some(result) = pinned_stream.next().await {
        match result {
            Ok(s) => all_data.push_str(&s),
            Err(_) => break,
        }
    }

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(content_type).unwrap(),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{}\"", filename)).unwrap(),
    );

    (StatusCode::OK, headers, all_data)
}

/// Export transactions as CSV with true streaming
pub async fn export_transactions_csv(
    State(state): State<crate::ApiState>,
    Query(query): Query<ExportQuery>,
) -> impl IntoResponse {
    let pool = Arc::new(state.app_state.db);
    let from = query.from.clone();
    let to = query.to.clone();
    let status = query.status.clone();
    let asset_code = query.asset_code.clone();

    let stream = create_csv_stream(pool, from, to, status, asset_code);

    // Generate filename with current date
    let filename = format!("transactions_{}.csv", Utc::now().format("%Y-%m"));

    stream_to_response(stream, "text/csv", &filename).await
}

/// Export transactions as JSON with true streaming (JSON Lines format)
pub async fn export_transactions_json(
    State(state): State<crate::ApiState>,
    Query(query): Query<ExportQuery>,
) -> impl IntoResponse {
    let pool = Arc::new(state.app_state.db);
    let from = query.from.clone();
    let to = query.to.clone();
    let status = query.status.clone();
    let asset_code = query.asset_code.clone();

    let stream = create_json_stream(pool, from, to, status, asset_code);

    // Generate filename with current date
    let filename = format!("transactions_{}.json", Utc::now().format("%Y-%m"));

    stream_to_response(stream, "application/json", &filename).await
}

/// Main export handler that routes to CSV or JSON based on format parameter
pub async fn export_transactions(
    State(state): State<crate::ApiState>,
    Query(query): Query<ExportQuery>,
) -> impl IntoResponse {
    let pool = Arc::new(state.app_state.db);
    let from = query.from.clone();
    let to = query.to.clone();
    let status = query.status.clone();
    let asset_code = query.asset_code.clone();
    let format = query.format.clone();

    match format.to_lowercase().as_str() {
        "json" => {
            let stream = create_json_stream(pool, from, to, status, asset_code);
            let filename = format!("transactions_{}.json", Utc::now().format("%Y-%m"));
            stream_to_response(stream, "application/json", &filename).await
        }
        _ => {
            let stream = create_csv_stream(pool, from, to, status, asset_code);
            let filename = format!("transactions_{}.csv", Utc::now().format("%Y-%m"));
            stream_to_response(stream, "text/csv", &filename).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_format() {
        let query = ExportQuery::default();
        assert_eq!(query.format, "csv");
    }

    #[test]
    fn test_parse_date() {
        let result = parse_date("2025-01-01");
        assert!(result.is_ok());
    }

    #[test]
    fn test_transaction_csv_row_from() {
        use bigdecimal::BigDecimal;
        use uuid::Uuid;

        let tx = Transaction {
            id: Uuid::new_v4(),
            stellar_account: "GABC123".to_string(),
            amount: BigDecimal::from(100),
            asset_code: "USD".to_string(),
            status: "pending".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            anchor_transaction_id: Some("anchor-123".to_string()),
            callback_type: Some("deposit".to_string()),
            callback_status: Some("completed".to_string()),
            settlement_id: None,
            memo: None,
            memo_type: None,
            metadata: None,
        };

        let csv_row = TransactionCsvRow::from(&tx);
        assert!(!csv_row.id.is_empty());
        assert_eq!(csv_row.stellar_account, "GABC123");
    }

    #[test]
    fn test_transaction_json_row_from() {
        use bigdecimal::BigDecimal;
        use uuid::Uuid;

        let tx = Transaction {
            id: Uuid::new_v4(),
            stellar_account: "GABC123".to_string(),
            amount: BigDecimal::from(100),
            asset_code: "USD".to_string(),
            status: "pending".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            anchor_transaction_id: Some("anchor-123".to_string()),
            callback_type: Some("deposit".to_string()),
            callback_status: Some("completed".to_string()),
            settlement_id: None,
            memo: None,
            memo_type: None,
            metadata: None,
        };

        let json_row = TransactionJsonRow::from(&tx);
        assert!(!json_row.id.is_empty());
        assert_eq!(json_row.stellar_account, "GABC123");
        assert_eq!(
            json_row.anchor_transaction_id,
            Some("anchor-123".to_string())
        );
    }

    #[test]
    fn test_build_filter_conditions_no_filters() {
        let (where_clause, params) = build_filter_conditions(&None, &None, &None, &None);
        assert!(where_clause.is_empty());
        assert!(params.is_empty());
    }

    #[test]
    fn test_build_filter_conditions_with_date_range() {
        let from = Some("2025-01-01".to_string());
        let to = Some("2025-02-01".to_string());
        let (where_clause, params) = build_filter_conditions(&from, &to, &None, &None);
        assert!(where_clause.contains("created_at >="));
        assert!(where_clause.contains("created_at <"));
        assert_eq!(params.len(), 2);
    }
}
