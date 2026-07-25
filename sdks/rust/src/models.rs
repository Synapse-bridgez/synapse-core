use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Cache metrics  (#899)
//
// The server exposes a single `/cache/metrics` endpoint that returns a
// *combined* view of two separate cache subsystems:
//
//  1. `query_cache`     — the in-process LRU query result cache
//                         (`CacheMetrics` from `services::query_cache`).
//  2. Idempotency cache — Redis-backed idempotency key store counters.
//
// This is **not** the same shape as the placeholder
// `sdks/rust/src/models::CacheMetrics { hits, misses, hit_rate, evictions,
// size, capacity }` that was previously mentioned in issue #899.  That struct
// targeted a hypothetical `/stats/cache` path which does not exist; the live
// server endpoint is `/cache/metrics` and its JSON shape matches the structs
// below.
//
// The SDK field names deliberately mirror the server's `CombinedCacheMetrics`
// and nested `CacheMetrics` types (see `src/handlers/stats.rs` and
// `src/services/query_cache.rs`) so that `serde` deserialization is zero-copy
// and requires no field renaming.
// ---------------------------------------------------------------------------

/// Inner query-cache metrics returned inside [`CombinedCacheMetrics::query_cache`].
///
/// Corresponds to `services::query_cache::CacheMetrics` on the server.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct QueryCacheMetrics {
    /// Number of cache hits since the last reset.
    pub hits: u64,
    /// Number of cache misses since the last reset.
    pub misses: u64,
    /// Hit rate as a ratio in `[0.0, 1.0]`.
    pub hit_rate: f64,
    /// Number of entries evicted from the LRU cache.
    pub evictions: u64,
    /// Current number of entries stored in the cache.
    pub size: u64,
    /// Maximum capacity of the LRU cache.
    pub capacity: u64,
}

/// Combined cache metrics returned by `GET /cache/metrics`.
///
/// This is the canonical SDK model for the server's `/cache/metrics` endpoint.
/// It aggregates metrics from the in-process query-result cache and the
/// Redis-backed idempotency-key store into a single response.
///
/// # Endpoint
/// `GET /cache/metrics`  (requires admin `Authorization: Bearer <key>`)
///
/// # Example
/// ```no_run
/// use synapse_sdk::SynapseClient;
///
/// # #[tokio::main]
/// # async fn main() {
/// let client = SynapseClient::new("https://api.example.com", "your-admin-key");
/// let metrics = client.stats().cache_metrics().await.unwrap();
/// println!("query cache hit rate: {:.2}%", metrics.query_cache.hit_rate * 100.0);
/// println!("idempotency hits: {}", metrics.idempotency_cache_hits);
/// # }
/// ```
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CombinedCacheMetrics {
    /// Metrics for the in-process LRU query-result cache.
    pub query_cache: QueryCacheMetrics,
    /// Total idempotency-cache hits recorded by the idempotency middleware.
    pub idempotency_cache_hits: u64,
    /// Total idempotency-cache misses.
    pub idempotency_cache_misses: u64,
    /// Number of times an idempotency lock was successfully acquired.
    pub idempotency_lock_acquired: u64,
    /// Number of times lock acquisition was contended (concurrent duplicate requests).
    pub idempotency_lock_contention: u64,
    /// Number of idempotency-processing errors.
    pub idempotency_errors: u64,
    /// Number of times the idempotency fallback path was taken.
    pub idempotency_fallback_count: u64,
}

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
