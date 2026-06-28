use crate::client::ApiClient;
use crate::formatter::{print, print_one, OutputFormat, TableDisplay};
use anyhow::Result;
use clap::Subcommand;
use serde::{Deserialize, Serialize};

// ── Response types (mirrors src/db/queries and src/handlers/stats.rs) ─────────

#[derive(Debug, Serialize, Deserialize)]
pub struct StatusCount {
    pub status: String,
    pub count: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DailyTotal {
    pub date: String,
    pub total_amount: String,
    pub transaction_count: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AssetStats {
    pub asset_code: String,
    pub total_amount: String,
    pub transaction_count: i64,
}

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
            self.transaction_count.to_string(),
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
            self.transaction_count.to_string(),
            self.total_amount.clone(),
        ]
    }
}

// ── Subcommand definitions ────────────────────────────────────────────────────

#[derive(Subcommand)]
pub enum StatsCommand {
    /// Transaction counts grouped by status (pending, completed, failed, …).
    ///
    /// Calls GET /stats/status. Results are cached server-side; stale data is
    /// possible on replicas (X-Read-Consistency: eventual header).
    /// Edge case: an empty dataset returns a valid zeroed list, never null.
    ///
    /// Example:
    ///   synapse stats status
    ///   synapse stats status --json
    Status {
        /// Print output as JSON instead of a table
        #[arg(long)]
        json: bool,
    },

    /// Daily transaction totals for the last N days (1–365, default 7).
    ///
    /// Calls GET /stats/daily?days=<N>.
    /// Edge case: an empty dataset returns a valid zeroed list, never null.
    ///
    /// Example:
    ///   synapse stats daily
    ///   synapse stats daily --days 30 --json
    Daily {
        /// Number of past days to include (1–365, default 7)
        #[arg(long, default_value = "7")]
        days: i32,

        /// Print output as JSON instead of a table
        #[arg(long)]
        json: bool,
    },

    /// Transaction totals grouped by asset code.
    ///
    /// Calls GET /stats/assets. Results are cached server-side.
    /// Edge case: an empty dataset returns a valid zeroed list, never null.
    ///
    /// Example:
    ///   synapse stats assets
    ///   synapse stats assets --json
    Assets {
        /// Print output as JSON instead of a table
        #[arg(long)]
        json: bool,
    },

    /// Cache hit/miss metrics for the query and idempotency caches.
    ///
    /// Calls GET /cache/metrics.
    ///
    /// Example:
    ///   synapse stats cache
    ///   synapse stats cache --json
    Cache {
        /// Print output as JSON instead of a table
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
            let fmt = if json { OutputFormat::Json } else { OutputFormat::Table };
            print(&items, fmt);
        }
        StatsCommand::Daily { days, json } => {
            let days_str = days.to_string();
            let items: Vec<DailyTotal> = client
                .get_with_query("/stats/daily", &[("days", &days_str)])
                .await?;
            let fmt = if json { OutputFormat::Json } else { OutputFormat::Table };
            print(&items, fmt);
        }
        StatsCommand::Assets { json } => {
            let items: Vec<AssetStats> = client.get("/stats/assets").await?;
            let fmt = if json { OutputFormat::Json } else { OutputFormat::Table };
            print(&items, fmt);
        }
        StatsCommand::Cache { json } => {
            let metrics: CacheMetrics = client.get("/cache/metrics").await?;
            let fmt = if json { OutputFormat::Json } else { OutputFormat::Table };
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
        assert!(items.is_empty(), "empty dataset must be an empty vec, not an error");
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
            .with_body(r#"[{"date":"2026-06-27","total_amount":"1000.00","transaction_count":5}]"#)
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let items: Vec<DailyTotal> = client
            .get_with_query("/stats/daily", &[("days", "7")])
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].transaction_count, 5);
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
            .get_with_query("/stats/daily", &[("days", "7")])
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
            .with_body(r#"[{"date":"2026-06-01","total_amount":"500.00","transaction_count":3}]"#)
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let items: Vec<DailyTotal> = client
            .get_with_query("/stats/daily", &[("days", "30")])
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
            .with_body(r#"[{"asset_code":"USD","total_amount":"9999.00","transaction_count":20}]"#)
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
            .with_body(r#"[{"asset_code":"EUR","total_amount":"200.00","transaction_count":2}]"#)
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

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;

    #[tokio::test]
    async fn stats_status_happy_path() {
        let mut server = Server::new_async().await;
        server.mock("GET", "/stats/status").with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"[{"status":"pending","count":5},{"status":"completed","count":10}]"#)
            .create_async().await;
        let client = ApiClient::new(&server.url(), "key");
        let items: Vec<StatusCount> = client.get("/stats/status").await.unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].count, 5);
    }

    #[tokio::test]
    async fn stats_status_empty_is_valid() {
        let mut server = Server::new_async().await;
        server.mock("GET", "/stats/status").with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"[]"#).create_async().await;
        let client = ApiClient::new(&server.url(), "key");
        let items: Vec<StatusCount> = client.get("/stats/status").await.unwrap();
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn stats_status_json_mode() {
        let mut server = Server::new_async().await;
        server.mock("GET", "/stats/status").with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"[{"status":"completed","count":3}]"#)
            .create_async().await;
        let client = ApiClient::new(&server.url(), "key");
        let items: Vec<StatusCount> = client.get("/stats/status").await.unwrap();
        let json = serde_json::to_string_pretty(&items).unwrap();
        assert!(json.contains("completed"));
    }

    #[tokio::test]
    async fn stats_daily_happy_path() {
        let mut server = Server::new_async().await;
        server.mock("GET", "/stats/daily").match_query(mockito::Matcher::UrlEncoded("days".into(), "7".into()))
            .with_status(200).with_header("content-type", "application/json")
            .with_body(r#"[{"date":"2026-06-27","total_amount":"500.00","transaction_count":10}]"#)
            .create_async().await;
        let client = ApiClient::new(&server.url(), "key");
        let items: Vec<DailyTotal> = client.get_with_query("/stats/daily", &[("days", "7")]).await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].transaction_count, 10);
    }

    #[tokio::test]
    async fn stats_daily_empty_is_valid() {
        let mut server = Server::new_async().await;
        server.mock("GET", "/stats/daily").with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"[]"#).create_async().await;
        let client = ApiClient::new(&server.url(), "key");
        let items: Vec<DailyTotal> = client.get_with_query("/stats/daily", &[("days", "7")]).await.unwrap();
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn stats_daily_json_mode() {
        let mut server = Server::new_async().await;
        server.mock("GET", "/stats/daily").with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"[{"date":"2026-06-27","total_amount":"100.00","transaction_count":2}]"#)
            .create_async().await;
        let client = ApiClient::new(&server.url(), "key");
        let items: Vec<DailyTotal> = client.get_with_query("/stats/daily", &[("days", "7")]).await.unwrap();
        let json = serde_json::to_string_pretty(&items).unwrap();
        assert!(json.contains("2026-06-27"));
    }

    #[tokio::test]
    async fn stats_assets_happy_path() {
        let mut server = Server::new_async().await;
        server.mock("GET", "/stats/assets").with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"[{"asset_code":"USD","total_amount":"1000.00","transaction_count":20}]"#)
            .create_async().await;
        let client = ApiClient::new(&server.url(), "key");
        let items: Vec<AssetStats> = client.get("/stats/assets").await.unwrap();
        assert_eq!(items[0].asset_code, "USD");
    }

    #[tokio::test]
    async fn stats_assets_empty_is_valid() {
        let mut server = Server::new_async().await;
        server.mock("GET", "/stats/assets").with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"[]"#).create_async().await;
        let client = ApiClient::new(&server.url(), "key");
        let items: Vec<AssetStats> = client.get("/stats/assets").await.unwrap();
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn stats_assets_json_mode() {
        let mut server = Server::new_async().await;
        server.mock("GET", "/stats/assets").with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"[{"asset_code":"XLM","total_amount":"50.00","transaction_count":5}]"#)
            .create_async().await;
        let client = ApiClient::new(&server.url(), "key");
        let items: Vec<AssetStats> = client.get("/stats/assets").await.unwrap();
        let json = serde_json::to_string_pretty(&items).unwrap();
        assert!(json.contains("XLM"));
    }

    #[tokio::test]
    async fn stats_cache_happy_path() {
        let mut server = Server::new_async().await;
        server.mock("GET", "/cache/metrics").with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"query_cache":{},"idempotency_cache_hits":10,"idempotency_cache_misses":2,"idempotency_lock_acquired":5,"idempotency_lock_contention":0,"idempotency_errors":0,"idempotency_fallback_count":0}"#)
            .create_async().await;
        let client = ApiClient::new(&server.url(), "key");
        let m: CacheMetrics = client.get("/cache/metrics").await.unwrap();
        assert_eq!(m.idempotency_cache_hits, 10);
    }

    #[tokio::test]
    async fn stats_cache_json_mode() {
        let mut server = Server::new_async().await;
        server.mock("GET", "/cache/metrics").with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"query_cache":{},"idempotency_cache_hits":0,"idempotency_cache_misses":0,"idempotency_lock_acquired":0,"idempotency_lock_contention":0,"idempotency_errors":0,"idempotency_fallback_count":0}"#)
            .create_async().await;
        let client = ApiClient::new(&server.url(), "key");
        let m: CacheMetrics = client.get("/cache/metrics").await.unwrap();
        let json = serde_json::to_string_pretty(&m).unwrap();
        assert!(json.contains("idempotency_cache_hits"));
    }

    #[tokio::test]
    async fn stats_server_error_returns_err() {
        let mut server = Server::new_async().await;
        server.mock("GET", "/stats/status").with_status(500)
            .with_body("internal error").create_async().await;
        let client = ApiClient::new(&server.url(), "key");
        let result: Result<Vec<StatusCount>> = client.get("/stats/status").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("500"));
    }
}
