use axum::{
    extract::{Path, State},
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::db::models::TransactionDlq;
use crate::error::AppError;
use crate::services::TransactionProcessor;

pub fn dlq_routes() -> Router<PgPool> {
    Router::new()
        .route("/dlq", get(list_dlq))
        .route("/dlq/:id/requeue", post(requeue_dlq))
}

/// A DLQ row that could not be deserialized into [`TransactionDlq`].
///
/// The raw row is retained (never dropped) so operators can inspect and
/// repair it; the payload is also logged and counted in
/// `dlq_quarantined_total`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct QuarantinedDlqEntry {
    pub id: Option<Uuid>,
    pub reason: String,
    pub raw: Value,
}

/// Convert a raw DLQ row into a [`TransactionDlq`], quarantining it on failure.
///
/// Returns `Ok(entry)` on success and `Err(quarantined)` when the row does not
/// match the expected schema. The quarantine counter is incremented and the
/// payload logged in the failure case.
pub fn deserialize_dlq_row(
    row: &sqlx::postgres::PgRow,
) -> Result<TransactionDlq, QuarantinedDlqEntry> {
    match TransactionDlq::from_row(row) {
        Ok(entry) => Ok(entry),
        Err(err) => {
            let raw = row_to_json(row);
            let id = raw
                .get("id")
                .and_then(Value::as_str)
                .and_then(|s| Uuid::parse_str(s).ok());
            let reason = err.to_string();

            crate::metrics::dlq_quarantined_total().add(1, &[]);
            tracing::error!(
                dlq_id = ?id,
                error = %reason,
                payload = %raw,
                "DLQ message failed to deserialize; quarantined raw payload"
            );

            Err(QuarantinedDlqEntry { id, reason, raw })
        }
    }
}

fn row_to_json(row: &sqlx::postgres::PgRow) -> Value {
    use sqlx::{Column, TypeInfo, ValueRef};

    let mut map = serde_json::Map::new();
    for (idx, column) in row.columns().iter().enumerate() {
        let name = column.name().to_string();
        let value = match row.try_get_raw(idx) {
            Ok(raw) if raw.is_null() => Value::Null,
            Ok(_) => {
                let type_name = column.type_info().name();
                match type_name {
                    "UUID" => row
                        .try_get::<Uuid, _>(idx)
                        .map(|v| Value::String(v.to_string())),
                    "TEXT" | "VARCHAR" | "BPCHAR" | "NAME" => {
                        row.try_get::<String, _>(idx).map(Value::String)
                    }
                    "INT4" => row.try_get::<i32, _>(idx).map(|v| json!(v)),
                    "INT8" => row.try_get::<i64, _>(idx).map(|v| json!(v)),
                    "BOOL" => row.try_get::<bool, _>(idx).map(|v| json!(v)),
                    "TIMESTAMPTZ" => row
                        .try_get::<chrono::DateTime<chrono::Utc>, _>(idx)
                        .map(|v| Value::String(v.to_rfc3339())),
                    _ => row.try_get::<String, _>(idx).map(Value::String),
                }
                .unwrap_or_else(|_| Value::String("<undecodable>".to_string()))
            }
            Err(_) => Value::String("<undecodable>".to_string()),
        };
        map.insert(name, value);
    }
    Value::Object(map)
}

async fn list_dlq(State(pool): State<PgPool>) -> Result<impl IntoResponse, AppError> {
    let rows = sqlx::query("SELECT * FROM transaction_dlq ORDER BY moved_to_dlq_at DESC LIMIT 100")
        .fetch_all(&pool)
        .await?;

    let mut entries = Vec::with_capacity(rows.len());
    let mut quarantined = Vec::new();
    for row in &rows {
        match deserialize_dlq_row(row) {
            Ok(entry) => entries.push(entry),
            Err(bad) => quarantined.push(bad),
        }
    }

    Ok(Json(json!({
        "dlq_entries": entries,
        "count": entries.len(),
        "quarantined_entries": quarantined,
        "quarantined_count": quarantined.len()
    })))
}

async fn requeue_dlq(
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let processor = TransactionProcessor::new(pool);
    processor.requeue_dlq(id).await?;

    Ok(Json(json!({
        "message": "DLQ entry requeued successfully",
        "dlq_id": id
    })))
}
