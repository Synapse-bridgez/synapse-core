use crate::client::SynapseClient;
use crate::error::SynapseError;
use crate::models::{CacheMetrics, DailyTotal, StatsAsset, StatusCount};

pub struct Stats<'a> {
    pub(crate) client: &'a SynapseClient,
}

impl<'a> Stats<'a> {
    /// Fetch transaction counts broken down by status (`GET /stats/status`).
    ///
    /// Returns an empty list when no transactions exist — never `None`.
    pub async fn status(&self) -> Result<Vec<StatusCount>, SynapseError> {
        self.client.get("/stats/status").await
    }

    /// Fetch daily transaction totals for the last N days (`GET /stats/daily`).
    ///
    /// `days` must be between 1 and 365. Returns an empty list for periods with
    /// no data — never `None`.
    pub async fn daily(&self, days: Option<i32>) -> Result<Vec<DailyTotal>, SynapseError> {
        match days {
            Some(d) => {
                let d_str = d.to_string();
                self.client
                    .get_query("/stats/daily", &[("days", d_str.as_str())])
                    .await
            }
            None => self.client.get("/stats/daily").await,
        }
    }

    /// Fetch per-asset aggregated statistics (`GET /stats/assets`).
    ///
    /// Returns an empty list when no assets exist — never `None`.
    pub async fn assets(&self) -> Result<Vec<StatsAsset>, SynapseError> {
        self.client.get("/stats/assets").await
    }

    /// Fetch combined cache metrics (`GET /cache/metrics`).
    ///
    /// All counters are `0` when the cache has seen no activity.
    pub async fn cache_metrics(&self) -> Result<CacheMetrics, SynapseError> {
        self.client.get("/cache/metrics").await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // ── status() ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn status_happy_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/stats/status"))
            .and(header("X-API-Key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"status": "pending", "count": 10},
                {"status": "completed", "count": 50}
            ])))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "test-key");
        let result = client.stats().status().await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let counts = result.unwrap();
        assert_eq!(counts.len(), 2);
        assert_eq!(counts[0].status, "pending");
        assert_eq!(counts[0].count, 10);
        assert_eq!(counts[1].status, "completed");
        assert_eq!(counts[1].count, 50);
    }

    #[tokio::test]
    async fn status_empty_dataset_returns_empty_list() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/stats/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "test-key");
        let result = client.stats().status().await;

        assert!(result.is_ok(), "empty dataset must return empty list, not error");
        assert!(result.unwrap().is_empty());
    }

    // ── daily() ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn daily_happy_path_with_days_param() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/stats/daily"))
            .and(query_param("days", "7"))
            .and(header("X-API-Key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"date": "2026-06-25", "total_amount": "1500.00", "tx_count": 30},
                {"date": "2026-06-24", "total_amount": "800.50", "tx_count": 15}
            ])))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "test-key");
        let result = client.stats().daily(Some(7)).await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let totals = result.unwrap();
        assert_eq!(totals.len(), 2);
        assert_eq!(totals[0].date, "2026-06-25");
        assert_eq!(totals[0].tx_count, 30);
    }

    #[tokio::test]
    async fn daily_empty_dataset_returns_empty_list() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/stats/daily"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "test-key");
        let result = client.stats().daily(None).await;

        assert!(result.is_ok(), "empty dataset must return empty list, not error");
        assert!(result.unwrap().is_empty());
    }

    // ── assets() ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn assets_happy_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/stats/assets"))
            .and(header("X-API-Key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"asset_code": "USD", "total_amount": "10000.00", "tx_count": 200, "avg_amount": "50.00"},
                {"asset_code": "EUR", "total_amount": "5000.00", "tx_count": 100, "avg_amount": "50.00"}
            ])))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "test-key");
        let result = client.stats().assets().await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let stats = result.unwrap();
        assert_eq!(stats.len(), 2);
        assert_eq!(stats[0].asset_code, "USD");
        assert_eq!(stats[0].tx_count, 200);
    }

    #[tokio::test]
    async fn assets_empty_dataset_returns_empty_list() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/stats/assets"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "test-key");
        let result = client.stats().assets().await;

        assert!(result.is_ok(), "empty dataset must return empty list, not error");
        assert!(result.unwrap().is_empty());
    }

    // ── cache_metrics() ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn cache_metrics_happy_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/cache/metrics"))
            .and(header("X-API-Key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "query_cache": {
                    "hits": 100,
                    "misses": 20,
                    "total": 120,
                    "hit_rate": 0.833,
                    "memory_hits": 80,
                    "memory_misses": 10,
                    "memory_total": 90,
                    "memory_hit_rate": 0.888
                },
                "idempotency_cache_hits": 50,
                "idempotency_cache_misses": 5,
                "idempotency_lock_acquired": 45,
                "idempotency_lock_contention": 2,
                "idempotency_errors": 0,
                "idempotency_fallback_count": 1
            })))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "test-key");
        let result = client.stats().cache_metrics().await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let m = result.unwrap();
        assert_eq!(m.query_cache.hits, 100);
        assert_eq!(m.idempotency_cache_hits, 50);
    }

    #[tokio::test]
    async fn cache_metrics_empty_returns_zeroed_struct() {
        let server = MockServer::start().await;
        // All fields absent — should deserialise with defaults of 0
        Mock::given(method("GET"))
            .and(path("/cache/metrics"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "test-key");
        let result = client.stats().cache_metrics().await;

        assert!(result.is_ok(), "empty response must yield zeroed struct, not error");
        let m = result.unwrap();
        assert_eq!(m.query_cache.hits, 0);
        assert_eq!(m.idempotency_cache_hits, 0);
    }
}
