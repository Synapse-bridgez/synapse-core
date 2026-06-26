use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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

/// Query parameters for [`Transactions::list`].
#[derive(Debug, Default)]
pub struct ListParams {
    pub cursor: Option<String>,
    pub limit: Option<i64>,
    pub from_date: Option<String>,
    pub to_date: Option<String>,
}

// ── Stats models ─────────────────────────────────────────────────────────────

/// Transaction count broken down by status.
///
/// An empty dataset returns a zeroed `StatusCount` list (never `null`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StatusCount {
    pub status: String,
    pub count: i64,
}

/// Aggregated transaction totals for a single day.
///
/// An empty dataset returns an empty list (never `null`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DailyTotal {
    pub date: String,
    pub total_amount: String,
    pub tx_count: i64,
}

/// Per-asset aggregated statistics.
///
/// An empty dataset returns an empty list (never `null`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StatsAsset {
    pub asset_code: String,
    pub total_amount: String,
    pub tx_count: i64,
    pub avg_amount: String,
}

/// Combined cache metrics returned by `GET /cache/metrics`.
///
/// All counters default to `0` when no cache activity has occurred.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CacheMetrics {
    #[serde(default)]
    pub query_cache: QueryCacheMetrics,
    #[serde(default)]
    pub idempotency_cache_hits: u64,
    #[serde(default)]
    pub idempotency_cache_misses: u64,
    #[serde(default)]
    pub idempotency_lock_acquired: u64,
    #[serde(default)]
    pub idempotency_lock_contention: u64,
    #[serde(default)]
    pub idempotency_errors: u64,
    #[serde(default)]
    pub idempotency_fallback_count: u64,
}

/// Inner query-cache metrics nested inside [`CacheMetrics`].
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct QueryCacheMetrics {
    #[serde(default)]
    pub hits: u64,
    #[serde(default)]
    pub misses: u64,
    #[serde(default)]
    pub total: u64,
    #[serde(default)]
    pub hit_rate: f64,
    #[serde(default)]
    pub memory_hits: u64,
    #[serde(default)]
    pub memory_misses: u64,
    #[serde(default)]
    pub memory_total: u64,
    #[serde(default)]
    pub memory_hit_rate: f64,
}

// ── Events / reconnect models ─────────────────────────────────────────────────

/// Body sent to `POST /reconnect`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconnectRequest {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_sequence: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub force_resync: Option<bool>,
}

/// Response returned by both `POST /reconnect` and `GET /reconnect/status`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReconnectionResponse {
    #[serde(rename = "type")]
    pub response_type: String,
    #[serde(default)]
    pub status: Option<ReconnectStatusDetail>,
    #[serde(default)]
    pub backoff_seconds: u64,
    #[serde(default)]
    pub requires_resync: bool,
    #[serde(default)]
    pub message: Option<String>,
}

/// The inner `status` field of a `ReconnectionResponse`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReconnectStatusDetail {
    pub status: String,
    pub session_id: Option<String>,
}

/// Response for `GET /reconnect/status` — simplified view of session state.
///
/// `active` is `false` and the other fields are `None` when no session exists,
/// satisfying the requirement that `reconnect_status()` never errors on no session.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReconnectStatusResponse {
    pub active: bool,
    pub session_id: Option<String>,
    pub backoff_seconds: Option<u64>,
    pub requires_resync: Option<bool>,
}
