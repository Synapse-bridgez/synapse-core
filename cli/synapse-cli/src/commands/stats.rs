use crate::client::ApiClient;
use crate::formatter::{print, print_one, OutputFormat, TableDisplay};
use anyhow::Result;
use clap::Subcommand;
use serde::{Deserialize, Serialize};

// ── Response types (mirrors src/db/queries and src/handlers/stats.rs) ─────────

use synapse_sdk::{AssetStats, DailyTotal, StatusCount};

#[derive(Debug, Serialize, Deserialize)]
pub struct CacheMetrics {
    pub query_cache: serde_json::Value,
    pub idempotency_cache_hits: u64,
    pub idempotency_cache_misses: u64,
    pub idempotency_lock_acquired: u64,
    pub idempotency_lock_contention: u64,
    pub idempotency_errors: u64,
    pub idempotency_fallback_count: u64,
}

// ── TableDisplay impls ────────────────────────────────────────────────────────

impl TableDisplay for StatusCount {
    fn headers() -> Vec<&'static str> {
        vec!["STATUS", "COUNT"]
    }
    fn row(&self) -> Vec<String> {
        vec![self.status.clone(), self.count.to_string()]
    }
}

impl TableDisplay for DailyTotal {
    fn headers() -> Vec<&'static str> {
        vec!["DATE", "TRANSACTIONS", "TOTAL AMOUNT"]
    }
    fn row(&self) -> Vec<String> {
        vec![
            self.date.clone(),
            self.count.to_string(),
            self.total_amount.clone(),
        ]
    }
}

impl TableDisplay for AssetStats {
    fn headers() -> Vec<&'static str> {
        vec!["ASSET", "TRANSACTIONS", "TOTAL AMOUNT"]
    }
    fn row(&self) -> Vec<String> {
        vec![
            self.asset_code.clone(),
            self.count.to_string(),
            self.total_amount.clone(),
        ]
    }
}

// ── Subcommand definitions ────────────────────────────────────────────────────

#[derive(Subcommand)]
pub enum StatsCommand {
    #[command(
        about = "Transaction counts grouped by status",
        long_about = "Show transaction counts grouped by status (pending, completed, failed, …).\n\n\
                      Calls GET /stats/status.\n\n\
                      Results are cached server-side; on read-replicas a short staleness window\n\
                      is possible (indicated by the X-Read-Consistency: eventual response header).\n\n\
                      Edge case: an empty dataset returns a valid empty list, never null.\n\n\
                      Flags:\n  \
                      --json    Print raw JSON instead of a table\n\n\
                      Examples:\n  \
                      synapse stats status\n  \
                      synapse stats status --json"
    )]
    Status {
        /// Print output as JSON instead of a table
        #[arg(long)]
        json: bool,
    },

    #[command(
        about = "Daily transaction totals for the last N days",
        long_about = "Show daily transaction totals for the last N days (1–365, default 7).\n\n\
                      Calls GET /stats/daily?days=<N>.\n\n\
                      Each row contains the calendar date, the total number of transactions\n\
                      processed that day, and the aggregate fiat amount across all assets.\n\n\
                      Edge case: an empty dataset returns a valid empty list, never null.\n\n\
                      Flags:\n  \
                      --days <N>    Number of past days to include (1–365, default: 7)\n  \
                      --json        Print raw JSON instead of a table\n\n\
                      Examples:\n  \
                      synapse stats daily\n  \
                      synapse stats daily --days 30\n  \
                      synapse stats daily --days 90 --json"
    )]
    Daily {
        /// Number of past days to include (1–365, default 7)
        #[arg(long, default_value = "7", value_name = "N")]
        days: i32,

        /// Print output as JSON instead of a table
        #[arg(long)]
        json: bool,
    },

    #[command(
        about = "Transaction totals grouped by asset code",
        long_about = "Show transaction totals grouped by asset code (USD, EUR, USDC, …).\n\n\
                      Calls GET /stats/assets.\n\n\
                      Each row contains the asset code, the total transaction count,\n\
                      and the aggregate amount processed for that asset.\n\n\
                      Results are cached server-side.\n\n\
                      Edge case: an empty dataset returns a valid empty list, never null.\n\n\
                      Flags:\n  \
                      --json    Print raw JSON instead of a table\n\n\
                      Examples:\n  \
                      synapse stats assets\n  \
                      synapse stats assets --json"
    )]
    Assets {
        /// Print output as JSON instead of a table
        #[arg(long)]
        json: bool,
    },

    #[command(
        about = "Cache hit/miss metrics for query and idempotency caches",
        long_about = "Show cache hit/miss metrics for the query cache and idempotency cache.\n\n\
                      Calls GET /cache/metrics.\n\n\
                      Fields returned:\n  \
                      query_cache              — Internal query cache statistics (hits, misses, size)\n  \
                      idempotency_cache_hits   — Idempotency key lookups served from cache\n  \
                      idempotency_cache_misses — Idempotency key lookups that missed the cache\n  \
                      idempotency_lock_acquired   — Distributed locks successfully acquired\n  \
                      idempotency_lock_contention — Requests that waited for a held lock\n  \
                      idempotency_errors          — Errors during idempotency key processing\n  \
                      idempotency_fallback_count  — Requests that fell back to DB after cache miss\n\n\
                      Flags:\n  \
                      --json    Print raw JSON instead of a key-value table\n\n\
                      Examples:\n  \
                      synapse stats cache\n  \
                      synapse stats cache --json"
    )]
    Cache {
        /// Print output as JSON instead of a key-value table
        #[arg(long)]
        json: bool,
    },
}

// ── Runner ────────────────────────────────────────────────────────────────────

pub async fn run(cmd: StatsCommand, base_url: &str, api_key: &str) -> Result<()> {
    let client = ApiClient::new(base_url, api_key);

    match cmd {
        StatsCommand::Status { json } => {
            let items: Vec<StatusCount> = client.get("/stats/status").await?;
            let fmt = if json {
                OutputFormat::Json
            } else {
                OutputFormat::Table
            };
            print(&items, fmt);
        }
        StatsCommand::Daily { days, json } => {
            let days_str = days.to_string();
            let items: Vec<DailyTotal> = client
                .get_query("/stats/daily", &[("days", &days_str)])
                .await?;
            let fmt = if json {
                OutputFormat::Json
            } else {
                OutputFormat::Table
            };
            print(&items, fmt);
        }
        StatsCommand::Assets { json } => {
            let items: Vec<AssetStats> = client.get("/stats/assets").await?;
            let fmt = if json {
                OutputFormat::Json
            } else {
                OutputFormat::Table
            };
            print(&items, fmt);
        }
        StatsCommand::Cache { json } => {
            let metrics: CacheMetrics = client.get("/cache/metrics").await?;
            let fmt = if json {
                OutputFormat::Json
            } else {
                OutputFormat::Table
            };
            print_one(&metrics, fmt);
        }
    }

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;

    // ── stats status ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn stats_status_happy_path_table() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/stats/status")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"[{"status":"pending","count":5},{"status":"completed","count":10}]"#)
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let items: Vec<StatusCount> = client.get("/stats/status").await.unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].status, "pending");
        assert_eq!(items[0].count, 5);
    }

    /// Edge case: empty dataset must return a valid empty list, not null/None.
    #[tokio::test]
    async fn stats_status_empty_dataset_is_valid() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/stats/status")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"[]"#)
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let items: Vec<StatusCount> = client.get("/stats/status").await.unwrap();
        assert!(
            items.is_empty(),
            "empty dataset must be an empty vec, not an error"
        );
    }

    #[tokio::test]
    async fn stats_status_json_mode() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/stats/status")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"[{"status":"completed","count":42}]"#)
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let items: Vec<StatusCount> = client.get("/stats/status").await.unwrap();
        let json = serde_json::to_string_pretty(&items).unwrap();
        assert!(json.contains("\"status\""));
        assert!(json.contains("completed"));
        assert!(json.contains("42"));
    }

    #[tokio::test]
    async fn stats_status_server_error_returns_err() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/stats/status")
            .with_status(500)
            .with_body("internal error")
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let result: Result<Vec<StatusCount>> = client.get("/stats/status").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("500"));
    }

    // ── stats daily ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn stats_daily_happy_path_table() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/stats/daily")
            .match_query(mockito::Matcher::UrlEncoded("days".into(), "7".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"[{"date":"2026-06-27","total_amount":"1000.00","count":5}]"#)
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let items: Vec<DailyTotal> = client
            .get_query("/stats/daily", &[("days", "7")])
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].count, 5);
    }

    /// Edge case: empty dataset must return a valid empty list.
    #[tokio::test]
    async fn stats_daily_empty_dataset_is_valid() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/stats/daily")
            .match_query(mockito::Matcher::UrlEncoded("days".into(), "7".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"[]"#)
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let items: Vec<DailyTotal> = client
            .get_query("/stats/daily", &[("days", "7")])
            .await
            .unwrap();
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn stats_daily_json_mode() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/stats/daily")
            .match_query(mockito::Matcher::UrlEncoded("days".into(), "30".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"[{"date":"2026-06-01","total_amount":"500.00","count":3}]"#)
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let items: Vec<DailyTotal> = client
            .get_query("/stats/daily", &[("days", "30")])
            .await
            .unwrap();
        let json = serde_json::to_string_pretty(&items).unwrap();
        assert!(json.contains("\"date\""));
        assert!(json.contains("500.00"));
    }

    // ── stats assets ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn stats_assets_happy_path_table() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/stats/assets")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"[{"asset_code":"USD","total_amount":"9999.00","count":20}]"#)
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let items: Vec<AssetStats> = client.get("/stats/assets").await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].asset_code, "USD");
    }

    /// Edge case: empty dataset must return a valid empty list.
    #[tokio::test]
    async fn stats_assets_empty_dataset_is_valid() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/stats/assets")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"[]"#)
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let items: Vec<AssetStats> = client.get("/stats/assets").await.unwrap();
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn stats_assets_json_mode() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/stats/assets")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"[{"asset_code":"EUR","total_amount":"200.00","count":2}]"#)
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let items: Vec<AssetStats> = client.get("/stats/assets").await.unwrap();
        let json = serde_json::to_string_pretty(&items).unwrap();
        assert!(json.contains("EUR"));
        assert!(json.contains("200.00"));
    }

    // ── stats cache ───────────────────────────────────────────────────────────

    fn cache_body() -> &'static str {
        r#"{
          "query_cache": {"hits":100,"misses":5,"size":50},
          "idempotency_cache_hits": 80,
          "idempotency_cache_misses": 2,
          "idempotency_lock_acquired": 60,
          "idempotency_lock_contention": 1,
          "idempotency_errors": 0,
          "idempotency_fallback_count": 0
        }"#
    }

    #[tokio::test]
    async fn stats_cache_happy_path_table() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/cache/metrics")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(cache_body())
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let metrics: CacheMetrics = client.get("/cache/metrics").await.unwrap();
        assert_eq!(metrics.idempotency_cache_hits, 80);
        assert_eq!(metrics.idempotency_errors, 0);
    }

    #[tokio::test]
    async fn stats_cache_json_mode() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/cache/metrics")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(cache_body())
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let metrics: CacheMetrics = client.get("/cache/metrics").await.unwrap();
        let json = serde_json::to_string_pretty(&metrics).unwrap();
        assert!(json.contains("idempotency_cache_hits"));
        assert!(json.contains("80"));
    }

    #[tokio::test]
    async fn stats_cache_server_error_returns_err() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/cache/metrics")
            .with_status(500)
            .with_body("error")
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let result: Result<CacheMetrics> = client.get("/cache/metrics").await;
        assert!(result.is_err());
    }
}
