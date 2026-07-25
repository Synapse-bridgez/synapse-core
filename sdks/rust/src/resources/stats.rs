use crate::client::SynapseClient;
use crate::error::SynapseError;
use crate::models::{AssetStats, CacheMetrics, DailyParams, DailyTotal, StatusCount};

/// Access the stats endpoints (`/stats/*`).
pub struct Stats<'a> {
    pub(crate) client: &'a SynapseClient,
}

impl<'a> Stats<'a> {
    /// Fetch per-status transaction counts (`GET /stats/status`).
    ///
    /// An empty dataset returns an empty `Vec`, never an error.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use synapse_sdk::SynapseClient;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let client = SynapseClient::new("https://api.example.com", "key");
    /// let counts = client.stats().status().await.unwrap();
    /// // Empty dataset: valid zeroed structure, not null/None.
    /// for c in &counts {
    ///     println!("{}: {}", c.status, c.count);
    /// }
    /// # }
    /// ```
    pub async fn status(&self) -> Result<Vec<StatusCount>, SynapseError> {
        self.client.get("/stats/status").await
    }

    /// Fetch per-day transaction volumes (`GET /stats/daily?days=N`).
    ///
    /// `days` must be 1–365; defaults to 7 on the server. An empty dataset
    /// returns an empty `Vec`, never an error.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use synapse_sdk::{SynapseClient};
    /// use synapse_sdk::models::DailyParams;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let client = SynapseClient::new("https://api.example.com", "key");
    /// let totals = client.stats().daily(DailyParams { days: Some(7) }).await.unwrap();
    /// for t in &totals {
    ///     println!("{}: {} txns, {} total", t.date, t.count, t.total_amount);
    /// }
    /// # }
    /// ```
    pub async fn daily(&self, params: DailyParams) -> Result<Vec<DailyTotal>, SynapseError> {
        match params.days {
            Some(d) => {
                let d = d.to_string();
                self.client
                    .get_query("/stats/daily", &[("days", d.as_str())])
                    .await
            }
            None => self.client.get("/stats/daily").await,
        }
    }

    /// Fetch per-asset statistics (`GET /stats/assets`).
    ///
    /// An empty dataset returns an empty `Vec`, never an error.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use synapse_sdk::SynapseClient;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let client = SynapseClient::new("https://api.example.com", "key");
    /// let stats = client.stats().assets().await.unwrap();
    /// for s in &stats {
    ///     println!("{}: {} txns", s.asset_code, s.count);
    /// }
    /// # }
    /// ```
    pub async fn assets(&self) -> Result<Vec<AssetStats>, SynapseError> {
        self.client.get("/stats/assets").await
    }

    /// Fetch cache metrics (`GET /stats/cache`).
    ///
    /// Returns a zeroed [`CacheMetrics`] when the cache is empty — never
    /// `null`/`None`.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use synapse_sdk::SynapseClient;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let client = SynapseClient::new("https://api.example.com", "key");
    /// let m = client.stats().cache_metrics().await.unwrap();
    /// println!("hit rate: {:.1}%", m.hit_rate * 100.0);
    /// # }
    /// ```
    pub async fn cache_metrics(&self) -> Result<CacheMetrics, SynapseError> {
        self.client.get("/stats/cache").await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn status_returns_empty_vec_on_empty_dataset() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/stats/status"))
            .and(header("X-API-Key", "k"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "k");
        let result = client.stats().status().await;
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[tokio::test]
    async fn status_returns_counts() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/stats/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "status": "pending", "count": 5 },
                { "status": "completed", "count": 10 }
            ])))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "k");
        let counts = client.stats().status().await.unwrap();
        assert_eq!(counts.len(), 2);
        assert_eq!(counts[0].status, "pending");
        assert_eq!(counts[1].count, 10);
    }

    #[tokio::test]
    async fn cache_metrics_returns_zeroed_on_empty() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/stats/cache"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "hits": 0, "misses": 0, "hit_rate": 0.0,
                "evictions": 0, "size": 0, "capacity": 0
            })))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "k");
        let m = client.stats().cache_metrics().await.unwrap();
        assert_eq!(m.hits, 0);
        assert_eq!(m.hit_rate, 0.0);
    }
}
