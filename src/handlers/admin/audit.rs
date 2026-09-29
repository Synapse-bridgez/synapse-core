use crate::db::cold_storage::{self, TierInfo};
use crate::db::queries::{search_audit_logs, AuditLogRow, AuditSearchParams};
use crate::error::AppError;
use crate::ApiState;
use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row as _;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Query params
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct AuditSearchQuery {
    pub actor: Option<String>,
    pub action: Option<String>,
    pub from_date: Option<DateTime<Utc>>,
    pub to_date: Option<DateTime<Utc>>,
    pub entity_type: Option<String>,
    /// Opaque cursor returned by a previous response.
    pub cursor: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    /// When `true` the endpoint streams a CSV file instead of JSON.
    #[serde(default)]
    pub export: bool,
}

fn default_limit() -> i64 {
    50
}

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct AuditSearchResponse {
    pub total: i64,
    pub data: Vec<AuditLogRow>,
    /// Opaque cursor to pass as `?cursor=` for the next page.
    /// `null` when there are no more results.
    pub next_cursor: Option<String>,
}

// ---------------------------------------------------------------------------
// Cursor encoding helpers
// ---------------------------------------------------------------------------

/// Encode `(timestamp, id)` into a URL-safe base64 string.
fn encode_cursor(ts: DateTime<Utc>, id: Uuid) -> String {
    let raw = format!("{},{}", ts.timestamp_nanos_opt().unwrap_or(0), id);
    URL_SAFE_NO_PAD.encode(raw.as_bytes())
}

/// Decode a cursor string back to `(timestamp, id)`.
fn decode_cursor(cursor: &str) -> Option<(DateTime<Utc>, Uuid)> {
    let bytes = URL_SAFE_NO_PAD.decode(cursor).ok()?;
    let s = std::str::from_utf8(&bytes).ok()?;
    let mut parts = s.splitn(2, ',');
    let nanos: i64 = parts.next()?.parse().ok()?;
    let id: Uuid = parts.next()?.parse().ok()?;
    let ts = DateTime::from_timestamp_nanos(nanos);
    Some((ts, id))
}

// ---------------------------------------------------------------------------
// CSV serialisation
// ---------------------------------------------------------------------------

fn rows_to_csv(rows: &[AuditLogRow]) -> Result<String, csv::Error> {
    let mut wtr = csv::Writer::from_writer(vec![]);
    wtr.write_record([
        "id",
        "entity_id",
        "entity_type",
        "action",
        "old_val",
        "new_val",
        "actor",
        "timestamp",
    ])?;
    for row in rows {
        wtr.write_record([
            row.id.to_string(),
            row.entity_id.to_string(),
            row.entity_type.clone(),
            row.action.clone(),
            row.old_val
                .as_ref()
                .map(|v| v.to_string())
                .unwrap_or_default(),
            row.new_val
                .as_ref()
                .map(|v| v.to_string())
                .unwrap_or_default(),
            row.actor.clone(),
            row.timestamp.to_rfc3339(),
        ])?;
    }
    wtr.flush()?;
    let inner = wtr
        .into_inner()
        .map_err(|e| csv::Error::from(std::io::Error::other(e.to_string())))?;
    Ok(String::from_utf8_lossy(&inner).into_owned())
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// GET /admin/audit/search
///
/// Search audit logs across all entities with optional filters.
/// Supports cursor-based pagination and CSV export via `?export=true`.
///
/// Admin authentication required (Bearer token).
pub async fn search_audit_logs_handler(
    State(state): State<ApiState>,
    Query(q): Query<AuditSearchQuery>,
) -> Result<Response, AppError> {
    crate::metrics::admin_audit_search_requests_total().add(1, &[]);

    let limit = q.limit.clamp(1, 500);

    // `decode_cursor` returns `Option<(DateTime<Utc>, Uuid)>` (an invalid
    // cursor is simply treated as "no cursor"), not a `Result` — `.map(...)`
    // therefore produces `Option<Option<_>>`, which `.flatten()` collapses.
    // The previous `.transpose().ok()` here never compiled (this file was
    // never mod-declared, so it never ran through the compiler) — `.transpose()`
    // only exists on `Option<Result<_, _>>`.
    let cursor = q.cursor.as_deref().and_then(decode_cursor);

    let params = AuditSearchParams {
        actor: q.actor.as_deref(),
        action: q.action.as_deref(),
        from_date: q.from_date,
        to_date: q.to_date,
        entity_type: q.entity_type.as_deref(),
        limit,
        cursor,
    };

    let (total, rows) = search_audit_logs(&state.app_state.db, &params)
        .await
        .map_err(|e| AppError::DatabaseError(e.to_string()))?;

    // CSV export path
    if q.export {
        let csv_body = rows_to_csv(&rows).map_err(|e| AppError::Internal(e.to_string()))?;
        return Ok((
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"audit_logs.csv\"",
                ),
            ],
            csv_body,
        )
            .into_response());
    }

    // Build next cursor from the last row
    let next_cursor = if rows.len() == limit as usize {
        rows.last().map(|r| encode_cursor(r.timestamp, r.id))
    } else {
        None
    };

    Ok(Json(AuditSearchResponse {
        total,
        data: rows,
        next_cursor,
    })
    .into_response())
}

// ---------------------------------------------------------------------------
// Unified (hot + cold) audit log query (#1285)
// ---------------------------------------------------------------------------

/// Query params for the unified audit log endpoint.
#[derive(Debug, Deserialize)]
pub struct UnifiedAuditQuery {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub entity_id: Option<Uuid>,
    pub entity_type: Option<String>,
    #[serde(default = "default_unified_limit")]
    pub limit: i64,
}

fn default_unified_limit() -> i64 {
    100
}

/// Response returned by `GET /admin/audit/unified`.
#[derive(Debug, Serialize)]
pub struct UnifiedAuditResponse {
    pub total: i64,
    pub data: Vec<crate::db::cold_storage::UnifiedAuditRow>,
    pub tier_info: TierInfo,
}

/// `GET /admin/audit/unified` — query audit logs transparently across hot and
/// cold storage tiers.
///
/// Accepts `from`, `to` (RFC 3339), optional `entity_id`, `entity_type`, and
/// `limit` (max 1000).
///
/// When the requested time range overlaps cold-archived data, the response
/// body includes `tier_info.cold_tier_touched = true` and
/// `tier_info.cold_latency_note` so callers can surface this to their users.
/// Cold-tier access is also logged as a structured trace event.
pub async fn query_unified_audit_logs_handler(
    State(state): State<ApiState>,
    Query(q): Query<UnifiedAuditQuery>,
) -> Result<Response, AppError> {
    // Check whether the range touches cold archives that have NOT yet been
    // rehydrated, and warn if so (results will be incomplete until rehydration
    // is run).
    let cold_overlap = cold_storage::check_cold_overlap(&state.app_state.db, q.from, q.to)
        .await
        .unwrap_or(false);

    if cold_overlap {
        // Cold data has been registered — it's already in cold_audit_logs.
        // `query_audit_logs` will include it via the unified view.
    } else {
        // No cold pointers registered for this range.  Check if any archive
        // file *covers* this range but hasn't been rehydrated yet.
        let archive_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM audit_log_archives \
             WHERE covers_from <= $2 AND covers_to >= $1)",
        )
        .bind(q.from)
        .bind(q.to)
        .fetch_one(&state.app_state.db)
        .await
        .unwrap_or(false);

        if archive_exists {
            cold_storage::warn_cold_data_not_rehydrated(q.from, q.to);
        }
    }

    let result = cold_storage::query_audit_logs(
        &state.app_state.db,
        q.from,
        q.to,
        q.entity_id,
        q.entity_type.as_deref(),
        q.limit,
    )
    .await
    .map_err(|e| AppError::DatabaseError(e.to_string()))?;

    Ok(Json(UnifiedAuditResponse {
        total: result.total,
        data: result.rows,
        tier_info: result.tier_info,
    })
    .into_response())
}

/// `GET /admin/audit/cold/pointers` — list all registered cold archive
/// pointers, i.e. which archive files have been rehydrated into
/// `cold_audit_logs` and are available for unified queries.
pub async fn list_cold_pointers_handler(
    State(state): State<ApiState>,
) -> Result<impl IntoResponse, AppError> {
    let pointers = cold_storage::list_cold_pointers(&state.app_state.db)
        .await
        .map_err(|e| AppError::DatabaseError(e.to_string()))?;

    Ok((
        StatusCode::OK,
        Json(serde_json::json!({
            "count": pointers.len(),
            "pointers": pointers,
        })),
    ))
}

// ---------------------------------------------------------------------------
// Tests (existing + new)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn test_cursor_roundtrip() {
        let ts = Utc.with_ymd_and_hms(2026, 4, 26, 12, 0, 0).unwrap();
        let id = Uuid::new_v4();
        let encoded = encode_cursor(ts, id);
        let (decoded_ts, decoded_id) = decode_cursor(&encoded).expect("decode should succeed");
        assert_eq!(decoded_id, id);
        // Nanosecond precision roundtrip
        assert_eq!(decoded_ts.timestamp_nanos_opt(), ts.timestamp_nanos_opt());
    }

    #[test]
    fn test_decode_cursor_invalid() {
        assert!(decode_cursor("not-valid-base64!!!").is_none());
        assert!(decode_cursor("").is_none());
    }

    #[test]
    fn test_rows_to_csv_empty() {
        let csv = rows_to_csv(&[]).expect("empty CSV should succeed");
        assert!(csv.contains("id,entity_id,entity_type"));
    }

    #[test]
    fn test_rows_to_csv_with_row() {
        let row = AuditLogRow {
            id: Uuid::nil(),
            entity_id: Uuid::nil(),
            entity_type: "transaction".into(),
            action: "status_update".into(),
            old_val: Some(serde_json::json!({"status": "pending"})),
            new_val: Some(serde_json::json!({"status": "completed"})),
            actor: "admin".into(),
            timestamp: Utc::now(),
        };
        let csv = rows_to_csv(&[row]).expect("CSV should succeed");
        assert!(csv.contains("status_update"));
        assert!(csv.contains("transaction"));
        assert!(csv.contains("admin"));
    }

    #[test]
    fn test_default_limit() {
        assert_eq!(default_limit(), 50);
    }
}
