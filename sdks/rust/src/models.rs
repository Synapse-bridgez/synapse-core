use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ListMeta {
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

/// Paginated list of transactions.
#[derive(Debug, Clone, Deserialize, Serialize)]
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
#[derive(Debug, Clone, Deserialize, Serialize)]
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
#[derive(Debug, Clone, Deserialize, Serialize)]
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
}

// ============================================================================
// Admin: Reconciliation Models
// ============================================================================

/// A reconciliation report summary returned by list or run operations.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReconciliationReportSummary {
    pub id: Uuid,
    pub generated_at: DateTime<Utc>,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub total_db_transactions: i32,
    pub total_chain_payments: i32,
    pub missing_on_chain_count: i32,
    pub orphaned_payments_count: i32,
    pub amount_mismatches_count: i32,
    pub has_discrepancies: bool,
}

/// Paginated list of reconciliation reports.
#[derive(Debug, Clone, Deserialize)]
pub struct ListReconciliationReports {
    pub reports: Vec<ReconciliationReportSummary>,
    pub total: i64,
    pub limit: i32,
    pub offset: i32,
}

/// Query parameters for listing reconciliation reports.
#[derive(Debug, Default)]
pub struct ListReportsParams {
    /// Maximum records per page (server default: 20).
    pub limit: Option<i32>,
    /// Number of records to skip.
    pub offset: Option<i32>,
}

/// A missing transaction detail in a reconciliation report.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MissingTransaction {
    pub id: Uuid,
    pub stellar_account: String,
    pub amount: String,
    pub asset_code: String,
    pub memo: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// An orphaned payment detail in a reconciliation report.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OrphanedPayment {
    pub payment_id: String,
    pub from: String,
    pub to: String,
    pub amount: String,
    pub asset_code: String,
    pub memo: Option<String>,
}

/// An amount mismatch detail in a reconciliation report.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AmountMismatch {
    pub transaction_id: Uuid,
    pub payment_id: String,
    pub db_amount: String,
    pub chain_amount: String,
    pub memo: Option<String>,
}

/// Full reconciliation report details.
#[derive(Debug, Clone, Deserialize)]
pub struct ReconciliationReportDetail {
    pub id: Uuid,
    pub generated_at: DateTime<Utc>,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub summary: ReconciliationSummary,
    pub missing_on_chain: Vec<MissingTransaction>,
    pub orphaned_payments: Vec<OrphanedPayment>,
    pub amount_mismatches: Vec<AmountMismatch>,
}

/// Summary statistics in a full reconciliation report.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReconciliationSummary {
    pub total_db_transactions: usize,
    pub total_chain_payments: usize,
    pub missing_on_chain_count: i32,
    pub orphaned_payments_count: i32,
    pub amount_mismatches_count: i32,
    pub has_discrepancies: bool,
}

/// Response from running a reconciliation.
#[derive(Debug, Clone, Deserialize)]
pub struct RunReconciliationResponse {
    pub message: String,
    pub report: ReconciliationReportSummary,
}

/// Request to run a reconciliation.
#[derive(Debug, Serialize)]
pub struct RunReconciliationRequest {
    pub account: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub period_hours: Option<i32>,
}

// ============================================================================
// Admin: Settlement Models
// ============================================================================

/// A settlement record as returned by the admin status-update endpoint.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AdminSettlement {
    pub id: Uuid,
    pub status: String,
    pub total_amount: String,
    pub reason: Option<String>,
    pub actor: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Request to update a settlement's status.
#[derive(Debug, Serialize)]
pub struct UpdateSettlementStatusRequest {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// New total amount — only meaningful when transitioning to "adjusted".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_total: Option<String>,
    /// Actor performing the change (defaults to "admin").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
}

// ============================================================================
// Admin: Locks Models
// ============================================================================

/// A single distributed lock currently held by the server.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ActiveLock {
    pub resource: String,
    pub token: String,
    pub acquired_at: u64,
    pub ttl_secs: u64,
    pub expected_duration_secs: u64,
    pub overdue: bool,
}

/// Response from `GET /admin/locks`.
#[derive(Debug, Clone, Deserialize)]
pub struct LocksListResponse {
    pub active_locks: Vec<ActiveLock>,
    pub total: i64,
    pub overdue: i64,
}

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
        Self {
            hits: 0,
            misses: 0,
            hit_rate: 0.0,
            evictions: 0,
            size: 0,
            capacity: 0,
        }
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

// ============================================================================
// Admin: Webhook Replay Models
// ============================================================================

/// Optional filters for [`AdminWebhookReplay::list_failed`].
#[derive(Debug, Default)]
pub struct ListFailedWebhooksFilters {
    /// Maximum records to return (server default: 50, max: 100).
    pub limit: Option<i64>,
    /// Number of records to skip for pagination.
    pub offset: Option<i64>,
    /// Filter by asset code (e.g. `"USD"`).
    pub asset_code: Option<String>,
    /// Inclusive start timestamp (RFC 3339, e.g. `"2024-01-01T00:00:00Z"`).
    pub from_date: Option<String>,
    /// Inclusive end timestamp (RFC 3339).
    pub to_date: Option<String>,
}

/// A single failed webhook delivery from the audit log.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FailedWebhookInfo {
    pub transaction_id: Uuid,
    pub stellar_account: String,
    pub amount: String,
    pub asset_code: String,
    pub anchor_transaction_id: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub last_error: Option<String>,
    pub retry_count: i32,
}

/// Response from `GET /admin/webhooks/failed`.
#[derive(Debug, Clone, Deserialize)]
pub struct FailedWebhooksResponse {
    pub total: i64,
    pub webhooks: Vec<FailedWebhookInfo>,
}

/// Result of a single replay attempt (individual or within a batch).
///
/// **Always inspect this struct** — even when the HTTP response is 200 the
/// `success` field may be `false` for individual items in a batch.
#[derive(Debug, Clone, Deserialize)]
pub struct WebhookReplayResult {
    pub transaction_id: Uuid,
    pub success: bool,
    pub message: String,
    pub dry_run: bool,
    pub replayed_at: Option<DateTime<Utc>>,
}

/// Response from `POST /admin/webhooks/replay/batch`.
///
/// The overall HTTP status is always 200. **Callers must inspect each entry
/// in `results`** — `successful` and `failed` are aggregate counts and some
/// items may have failed even when others succeeded.
#[derive(Debug, Clone, Deserialize)]
pub struct BatchWebhookReplayResponse {
    pub total: usize,
    pub successful: usize,
    pub failed: usize,
    pub results: Vec<WebhookReplayResult>,
}

// ── Internal request types (not part of the public API surface) ───────────────

#[derive(Debug, Serialize)]
pub(crate) struct ReplayWebhookRequest {
    pub dry_run: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct BatchReplayRequest {
    pub transaction_ids: Vec<Uuid>,
    pub dry_run: bool,
}

// ============================================================================
// Admin: DLQ (Dead-Letter Queue) Models
// ============================================================================

/// A single dead-letter queue entry.
///
/// Mirrors `TransactionDlq` in `src/db/models.rs`. `amount` is returned as a
/// decimal string rather than a numeric type so callers can format it without
/// precision loss.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DlqEntry {
    pub id: Uuid,
    pub transaction_id: Uuid,
    pub stellar_account: String,
    pub amount: String,
    pub asset_code: String,
    pub anchor_transaction_id: Option<String>,
    pub error_reason: String,
    pub stack_trace: Option<String>,
    pub retry_count: i32,
    pub original_created_at: DateTime<Utc>,
    pub moved_to_dlq_at: DateTime<Utc>,
    pub last_retry_at: Option<DateTime<Utc>>,
}

/// Response from `GET /dlq`.
#[derive(Debug, Clone, Deserialize)]
pub struct DlqListResponse {
    pub dlq_entries: Vec<DlqEntry>,
    pub count: usize,
}

/// Response from `POST /dlq/:id/requeue`.
#[derive(Debug, Clone, Deserialize)]
pub struct RequeueResponse {
    pub message: String,
    pub dlq_id: Uuid,
}
