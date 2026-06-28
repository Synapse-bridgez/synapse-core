use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ── Transaction models ────────────────────────────────────────────────────────

/// A single transaction returned by the API.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Transaction {
    pub id: String,
    pub stellar_account: String,
    pub amount: String,
    pub asset_code: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub anchor_transaction_id: Option<String>,
    pub callback_type: Option<String>,
    pub callback_status: Option<String>,
    pub settlement_id: Option<String>,
    pub memo: Option<String>,
    pub memo_type: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

/// Pagination metadata included in list responses.
#[derive(Debug, Clone, Deserialize)]
pub struct ListMeta {
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

/// Paginated list of transactions.
#[derive(Debug, Clone, Deserialize)]
pub struct TransactionList {
    pub data: Vec<Transaction>,
    pub meta: ListMeta,
}

/// Filters for [`Transactions::search`].
#[derive(Debug, Default)]
pub struct SearchParams {
    pub status: Option<String>,
    pub asset_code: Option<String>,
    pub min_amount: Option<String>,
    pub max_amount: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub stellar_account: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

/// A single page of transactions returned by [`Transactions::search`].
#[derive(Debug, Clone, Deserialize)]
pub struct TransactionSearch {
    pub total: i64,
    #[serde(default)]
    pub results: Vec<Transaction>,
    #[serde(default)]
    pub next_cursor: Option<String>,
}

/// A single settlement returned by the API.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Settlement {
    pub id: String,
    pub asset_code: String,
    pub total_amount: String,
    pub tx_count: i32,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub dispute_reason: Option<String>,
    pub original_total_amount: Option<String>,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
}

/// Paginated list of settlements.
#[derive(Debug, Clone, Deserialize)]
pub struct SettlementList {
    pub settlements: Vec<Settlement>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

/// Query parameters for [`Settlements::list`].
///
/// All fields are optional; omit a field to accept the server's default.
#[derive(Debug, Default)]
pub struct SettlementParams {
    /// Opaque pagination cursor from a previous response's `next_cursor`.
    pub cursor: Option<String>,
    /// Maximum records per page (server default: 10, max: 100).
    pub limit: Option<i64>,
    /// Sort direction: `"forward"` (default) or `"backward"`.
    pub direction: Option<String>,
}

/// Query parameters for [`Transactions::list`].
#[derive(Debug, Default)]
pub struct ListParams {
    pub cursor: Option<String>,
    pub limit: Option<i64>,
    pub from_date: Option<String>,
    pub to_date: Option<String>,
}

/// Filters for [`Transactions::export`].
///
/// The SDK returns raw export bytes unchanged so callers can process CSV or
/// JSON themselves.
#[derive(Debug, Default)]
pub struct TransactionExportFilters {
    /// Export format, either `csv` or `json`. Defaults to CSV.
    pub format: Option<String>,
    /// Inclusive start date filter (YYYY-MM-DD).
    pub from: Option<String>,
    /// Inclusive end date filter (YYYY-MM-DD).
    pub to: Option<String>,
    /// Transaction status filter.
    pub status: Option<String>,
    /// Asset code filter.
    pub asset_code: Option<String>,
// ── GraphQL models (issue #634) ───────────────────────────────────────────────

/// Request body for `POST /graphql`.
#[derive(Debug, Clone, Serialize)]
pub struct GraphQLRequest {
    pub query: String,
    pub variables: Option<serde_json::Value>,
}

/// A GraphQL application-level error returned inside an HTTP 200 response.
#[derive(Debug, Clone, Deserialize)]
pub struct GraphQLError {
    pub message: String,
}

/// Response from `POST /graphql`.
#[derive(Debug, Clone, Deserialize)]
pub struct GraphQLResponse {
    pub data: Option<serde_json::Value>,
    #[serde(default)]
    pub errors: Vec<GraphQLError>,
}

// ── Stats models (issue #633) ─────────────────────────────────────────────────

/// Per-status transaction count returned by `GET /stats/status`.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct StatusCount {
    pub status: String,
    pub count: i64,
}

/// Per-day transaction volume returned by `GET /stats/daily`.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct DailyTotal {
    pub date: String,
    pub count: i64,
    pub total_amount: String,
}

/// Per-asset statistics returned by `GET /stats/assets`.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct AssetStats {
    pub asset_code: String,
    pub count: i64,
    pub total_amount: String,
}

/// Cache metrics returned by `GET /stats/cache`.
///
/// Empty datasets return a zeroed structure, never `null`/`None`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CacheMetrics {
    pub hits: u64,
    pub misses: u64,
    pub hit_rate: f64,
    pub evictions: u64,
    pub size: u64,
    pub capacity: u64,
}

impl Default for CacheMetrics {
    fn default() -> Self {
        Self { hits: 0, misses: 0, hit_rate: 0.0, evictions: 0, size: 0, capacity: 0 }
    }
}

/// Query parameters for `stats.daily()`.
#[derive(Debug, Default)]
pub struct DailyParams {
    /// Number of days to include (1–365; server default: 7).
    pub days: Option<i32>,
}

// ── Events / reconnect models (issue #642) ────────────────────────────────────

/// Response from `POST /reconnect` or `GET /reconnect/status`.
#[derive(Debug, Clone, Deserialize)]
pub struct ReconnectResponse {
    #[serde(rename = "type")]
    pub kind: String,
    pub status: Option<ReconnectStatus>,
    pub backoff_seconds: Option<u64>,
    pub requires_resync: Option<bool>,
    pub message: Option<String>,
}

/// Reconnect status variant embedded in [`ReconnectResponse`].
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ReconnectStatus {
    Ready { session_id: String },
    RetryAfter { wait_seconds: u64 },
    SessionExpired,
    InvalidToken,
}

// ── Admin / bulk-status models (issue #644) ───────────────────────────────────

/// Per-item failure reported inside [`BulkStatusResponse`].
#[derive(Debug, Clone, Deserialize)]
pub struct BulkUpdateError {
    pub id: String,
    pub error: String,
}

/// Response from `POST /admin/transactions/bulk-status`.
#[derive(Debug, Clone, Deserialize)]
pub struct BulkStatusResponse {
    pub updated: usize,
    pub failed: usize,
    #[serde(default)]
    pub errors: Vec<BulkUpdateError>,
}
