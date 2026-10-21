//! Swap API handlers: tenant analytics (#1506) and operator surveillance /
//! archive endpoints (#1505, #1508). Routed via `swap::versioning`.

use crate::error::AppError;
use crate::swap::analytics::Granularity;
use crate::swap::market_data::{self, PriceQuery, QuoteQuery};
use crate::tenant::TenantContext;
use crate::ApiState;
use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{types::BigDecimal, PgPool};
use uuid::Uuid;

// ── Tenant analytics ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct AnalyticsQuery {
    pub granularity: Granularity,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub pair: Option<String>,
    pub direction: Option<String>,
    pub status: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct BucketRow {
    pub bucket_start: DateTime<Utc>,
    pub pair: String,
    pub direction: String,
    pub status: String,
    pub order_count: i64,
    pub volume: BigDecimal,
    pub avg_slippage_bps: Option<BigDecimal>,
    pub median_settlement_ms: Option<f64>,
    #[sqlx(skip)]
    pub complete: bool,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Summary {
    pub filled: Option<i64>,
    pub failed: Option<i64>,
    pub volume: Option<BigDecimal>,
}

#[derive(Debug, Serialize)]
pub struct Freshness {
    pub as_of: DateTime<Utc>,
    /// Buckets starting at/after this instant may still change.
    pub incomplete_from: DateTime<Utc>,
    pub note: &'static str,
}

#[derive(Debug, Serialize)]
pub struct AnalyticsResponse {
    pub granularity: Granularity,
    pub timezone: &'static str,
    pub freshness: Freshness,
    pub success_rate: Option<f64>,
    pub summary: Summary,
    pub limit: i64,
    pub offset: i64,
    pub data: Vec<BucketRow>,
}

/// GET /swap/v1/analytics — always scoped to the caller's own tenant.
pub async fn tenant_analytics(
    State(state): State<ApiState>,
    tenant: TenantContext,
    Query(q): Query<AnalyticsQuery>,
) -> Result<Json<AnalyticsResponse>, AppError> {
    if q.to <= q.from {
        return Err(AppError::BadRequest("'to' must be after 'from'".into()));
    }
    if q.to - q.from > q.granularity.max_range() {
        return Err(AppError::BadRequest(format!(
            "range too large for {} granularity (max {} days)",
            q.granularity.as_str(),
            q.granularity.max_range().num_days()
        )));
    }
    let (limit, offset) = (
        q.limit.unwrap_or(100).clamp(1, 500),
        q.offset.unwrap_or(0).max(0),
    );
    let db = &state.app_state.db;
    const FILTER: &str = "tenant_id = $1 AND granularity = $2 AND bucket_start >= $3 AND bucket_start < $4
        AND ($5::text IS NULL OR pair = $5) AND ($6::text IS NULL OR direction = $6) AND ($7::text IS NULL OR status = $7)";
    let mut data: Vec<BucketRow> = sqlx::query_as(&format!(
        "SELECT bucket_start, pair, direction, status, order_count, volume, avg_slippage_bps, median_settlement_ms
         FROM swap_analytics_buckets WHERE {FILTER} ORDER BY bucket_start, pair, direction, status LIMIT $8 OFFSET $9"
    ))
    .bind(tenant.tenant_id)
    .bind(q.granularity.as_str())
    .bind(q.from)
    .bind(q.to)
    .bind(&q.pair)
    .bind(&q.direction)
    .bind(&q.status)
    .bind(limit)
    .bind(offset)
    .fetch_all(db)
    .await
    .map_err(AppError::Database)?;
    let now = Utc::now();
    for r in &mut data {
        r.complete = q.granularity.is_complete(r.bucket_start, now);
    }
    let summary: Summary = sqlx::query_as(&format!(
        "SELECT SUM(order_count) FILTER (WHERE status = 'filled')::bigint AS filled,
                SUM(order_count) FILTER (WHERE status IN ('failed','rejected'))::bigint AS failed,
                SUM(volume) AS volume
         FROM swap_analytics_buckets WHERE {FILTER}"
    ))
    .bind(tenant.tenant_id)
    .bind(q.granularity.as_str())
    .bind(q.from)
    .bind(q.to)
    .bind(&q.pair)
    .bind(&q.direction)
    .bind(&q.status)
    .fetch_one(db)
    .await
    .map_err(AppError::Database)?;
    let (f, x) = (summary.filled.unwrap_or(0), summary.failed.unwrap_or(0));
    Ok(Json(AnalyticsResponse {
        granularity: q.granularity,
        timezone: "UTC",
        freshness: Freshness {
            as_of: now,
            incomplete_from: q.granularity.bucket_start(now),
            note: "Buckets containing the current time may be incomplete and can change as late events arrive.",
        },
        success_rate: (f + x > 0).then(|| f as f64 / (f + x) as f64),
        summary,
        limit,
        offset,
        data,
    }))
}

// ── Operator: surveillance findings ──────────────────────────────────────────

fn actor(headers: &HeaderMap) -> String {
    headers
        .get("x-reviewer")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("admin")
        .to_string()
}

async fn audit(
    db: &PgPool,
    finding: Option<Uuid>,
    actor: &str,
    action: &str,
    from: Option<&str>,
    to: Option<&str>,
    note: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query("INSERT INTO surveillance_audit_log (finding_id, actor, action, from_status, to_status, note) VALUES ($1,$2,$3,$4,$5,$6)")
        .bind(finding)
        .bind(actor)
        .bind(action)
        .bind(from)
        .bind(to)
        .bind(note)
        .execute(db)
        .await
        .map_err(AppError::Database)?;
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct FindingsQuery {
    pub status: Option<String>,
    pub severity: Option<String>,
    pub tenant_id: Option<Uuid>,
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct FindingRow {
    pub id: Uuid,
    pub rule: String,
    pub severity: String,
    pub status: String,
    pub tenant_ids: Vec<Uuid>,
    pub accounts: Vec<String>,
    pub pair: Option<String>,
    pub route: Option<String>,
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    pub evidence: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

/// GET /admin/swap/v1/surveillance/findings — cross-tenant; admin-only and
/// every query is itself written to the audit log.
pub async fn list_findings(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(q): Query<FindingsQuery>,
) -> Result<Json<Vec<FindingRow>>, AppError> {
    let db = &state.app_state.db;
    let rows = sqlx::query_as(
        "SELECT id, rule, severity, status, tenant_ids, accounts, pair, route, window_start, window_end, evidence, created_at
         FROM surveillance_findings
         WHERE ($1::text IS NULL OR status = $1) AND ($2::text IS NULL OR severity = $2) AND ($3::uuid IS NULL OR $3 = ANY(tenant_ids))
         ORDER BY created_at DESC LIMIT $4",
    )
    .bind(&q.status)
    .bind(&q.severity)
    .bind(q.tenant_id)
    .bind(q.limit.unwrap_or(100).clamp(1, 500))
    .fetch_all(db)
    .await
    .map_err(AppError::Database)?;
    let note = format!(
        "status={:?} severity={:?} tenant={:?}",
        q.status, q.severity, q.tenant_id
    );
    audit(db, None, &actor(&headers), "query", None, None, Some(&note)).await?;
    Ok(Json(rows))
}

#[derive(Debug, Deserialize)]
pub struct UpdateStatus {
    pub status: String,
    pub note: Option<String>,
}

/// POST /admin/swap/v1/surveillance/findings/:id/status
pub async fn update_finding_status(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateStatus>,
) -> Result<Json<serde_json::Value>, AppError> {
    if !matches!(body.status.as_str(), "open" | "acknowledged" | "dismissed") {
        return Err(AppError::BadRequest(
            "status must be open, acknowledged or dismissed".into(),
        ));
    }
    let db = &state.app_state.db;
    let prev: Option<(String,)> =
        sqlx::query_as("SELECT status FROM surveillance_findings WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(db)
            .await
            .map_err(AppError::Database)?;
    let Some((from,)) = prev else {
        return Err(AppError::NotFound(format!("finding {id}")));
    };
    sqlx::query("UPDATE surveillance_findings SET status = $2, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(&body.status)
        .execute(db)
        .await
        .map_err(AppError::Database)?;
    audit(
        db,
        Some(id),
        &actor(&headers),
        "status_change",
        Some(&from),
        Some(&body.status),
        body.note.as_deref(),
    )
    .await?;
    Ok(Json(serde_json::json!({ "id": id, "status": body.status })))
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct AuditRow {
    pub actor: String,
    pub action: String,
    pub from_status: Option<String>,
    pub to_status: Option<String>,
    pub note: Option<String>,
    pub at: DateTime<Utc>,
}

/// GET /admin/swap/v1/surveillance/findings/:id/audit
pub async fn finding_audit(
    State(state): State<ApiState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<AuditRow>>, AppError> {
    let rows = sqlx::query_as("SELECT actor, action, from_status, to_status, note, at FROM surveillance_audit_log WHERE finding_id = $1 ORDER BY at")
        .bind(id)
        .fetch_all(&state.app_state.db)
        .await
        .map_err(AppError::Database)?;
    Ok(Json(rows))
}

// ── Operator: market data archive ────────────────────────────────────────────

/// GET /admin/swap/v1/market-data/quotes
pub async fn archived_quotes(
    State(state): State<ApiState>,
    Query(q): Query<QuoteQuery>,
) -> Result<Json<Vec<market_data::QuoteRecord>>, AppError> {
    market_data::query_quotes(&state.app_state.db, &q)
        .await
        .map(Json)
        .map_err(AppError::Database)
}

/// GET /admin/swap/v1/market-data/prices
pub async fn archived_prices(
    State(state): State<ApiState>,
    Query(q): Query<PriceQuery>,
) -> Result<Json<Vec<market_data::PriceSnapshot>>, AppError> {
    market_data::query_prices(&state.app_state.db, &q)
        .await
        .map(Json)
        .map_err(AppError::Database)
}
