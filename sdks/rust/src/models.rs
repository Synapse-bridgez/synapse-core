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
///
/// All fields are optional; omit a field to leave that dimension unfiltered.
/// A search with no matches returns an empty [`TransactionSearch`] (a page with
/// `total == 0` and no `results`), never an error.
#[derive(Debug, Default)]
pub struct SearchParams {
    /// Exact transaction status (e.g. `"pending"`, `"completed"`).
    pub status: Option<String>,
    /// Exact asset code (e.g. `"USD"`).
    pub asset_code: Option<String>,
    /// Inclusive minimum amount, as a decimal string (e.g. `"10.00"`).
    pub min_amount: Option<String>,
    /// Inclusive maximum amount, as a decimal string (e.g. `"500.00"`).
    pub max_amount: Option<String>,
    /// Inclusive RFC 3339 range start (e.g. `"2024-01-01T00:00:00Z"`).
    pub from: Option<String>,
    /// Exclusive RFC 3339 range end (e.g. `"2024-02-01T00:00:00Z"`).
    pub to: Option<String>,
    /// Exact Stellar account to filter by.
    pub stellar_account: Option<String>,
    /// Opaque pagination cursor from a previous response's `next_cursor`.
    pub cursor: Option<String>,
    /// Maximum records per page (server default: 25, max: 100).
    pub limit: Option<i64>,
}

/// A single page of transactions returned by [`Transactions::search`].
#[derive(Debug, Clone, Deserialize)]
pub struct TransactionSearch {
    /// Total number of records matching the filters across all pages.
    pub total: i64,
    /// Matching transactions for this page (empty when nothing matched).
    #[serde(default)]
    pub results: Vec<Transaction>,
    /// Opaque cursor for the next page, or `None` when this is the last page.
    #[serde(default)]
    pub next_cursor: Option<String>,
}

/// Query parameters for [`Transactions::list`].
///
/// All fields are optional; omit a field to accept the server's default.
/// Never construct a `cursor` manually — always use one from a previous
/// response's `meta.next_cursor`.
#[derive(Debug, Default)]
pub struct ListParams {
    /// Opaque pagination cursor from `meta.next_cursor`.
    pub cursor: Option<String>,
    /// Maximum records per page (server default: 25, max: 100).
    pub limit: Option<i64>,
    /// Inclusive ISO 8601 range start (e.g. `"2024-01-01T00:00:00Z"`).
    pub from_date: Option<String>,
    /// Exclusive ISO 8601 range end (e.g. `"2024-02-01T00:00:00Z"`).
    pub to_date: Option<String>,
}
