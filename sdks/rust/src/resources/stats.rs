//! `Stats` resource: wrappers around the server's statistics endpoints.
//!
//! # Endpoint alignment  (#899)
//!
//! The server exposes the following stats-related routes:
//!
//! | Route                | Handler                          | Auth          |
//! |----------------------|----------------------------------|---------------|
//! | `GET /stats/status`  | `handlers::stats::status_counts` | admin bearer  |
//! | `GET /stats/daily`   | `handlers::stats::daily_totals`  | admin bearer  |
//! | `GET /stats/assets`  | `handlers::stats::asset_stats`   | admin bearer  |
//! | `GET /cache/metrics` | `handlers::stats::cache_metrics` | admin bearer  |
//!
//! Note: there is **no** `/stats/cache` endpoint.  Issue #899 identified that
//! an earlier SDK draft called `GET /stats/cache` and deserialised into a
//! `CacheMetrics { hits, misses, hit_rate, evictions, size, capacity }` struct
//! — a shape that does not match anything the server produces.  This module
//! uses the correct path (`/cache/metrics`) and the correct response type
//! ([`CombinedCacheMetrics`]).

use crate::client::SynapseClient;
use crate::error::SynapseError;
use crate::models::CombinedCacheMetrics;

/// Access to the server's statistics and metrics endpoints.
///
/// Obtain a handle via [`SynapseClient::stats`].
pub struct Stats<'a> {
    pub(crate) client: &'a SynapseClient,
}

impl<'a> Stats<'a> {
    /// Fetch combined cache metrics from `GET /cache/metrics`.
    ///
    /// Returns a [`CombinedCacheMetrics`] that contains metrics from both the
    /// in-process query-result LRU cache and the Redis-backed idempotency-key
    /// store.
    ///
    /// This endpoint requires an admin bearer token
    /// (`Authorization: Bearer <admin-key>`).
    ///
    /// # Errors
    /// - [`SynapseError::Http`] – server returned a non-2xx status (e.g. 401
    ///   when the bearer token is missing or invalid).
    /// - [`SynapseError::Network`] – network error before a response arrived.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use synapse_sdk::SynapseClient;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let client = SynapseClient::new("https://api.example.com", "admin-key");
    /// let metrics = client.stats().cache_metrics().await.unwrap();
    /// println!(
    ///     "query-cache hit rate: {:.1}%",
    ///     metrics.query_cache.hit_rate * 100.0
    /// );
    /// println!("idempotency hits: {}", metrics.idempotency_cache_hits);
    /// # }
    /// ```
    pub async fn cache_metrics(&self) -> Result<CombinedCacheMetrics, SynapseError> {
        self.client.get("/cache/metrics").await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Helper that builds a valid JSON body matching the server's
    /// `CombinedCacheMetrics` shape (see `src/handlers/stats.rs`).
    fn combined_cache_metrics_body() -> serde_json::Value {
        serde_json::json!({
            "query_cache": {
                "hits": 120,
                "misses": 30,
                "hit_rate": 0.8,
                "evictions": 5,
                "size": 200,
                "capacity": 1000
            },
            "idempotency_cache_hits": 42,
            "idempotency_cache_misses": 8,
            "idempotency_lock_acquired": 50,
            "idempotency_lock_contention": 2,
            "idempotency_errors": 0,
            "idempotency_fallback_count": 1
        })
    }

    #[tokio::test]
    async fn cache_metrics_calls_correct_endpoint() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/cache/metrics"))
            .and(header("X-API-Key", "admin-key"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(combined_cache_metrics_body()),
            )
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "admin-key");
        let result = client.stats().cache_metrics().await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let m = result.unwrap();
        assert_eq!(m.query_cache.hits, 120);
        assert_eq!(m.query_cache.misses, 30);
        assert!((m.query_cache.hit_rate - 0.8).abs() < f64::EPSILON);
        assert_eq!(m.query_cache.evictions, 5);
        assert_eq!(m.query_cache.size, 200);
        assert_eq!(m.query_cache.capacity, 1000);
        assert_eq!(m.idempotency_cache_hits, 42);
        assert_eq!(m.idempotency_cache_misses, 8);
        assert_eq!(m.idempotency_lock_acquired, 50);
        assert_eq!(m.idempotency_lock_contention, 2);
        assert_eq!(m.idempotency_errors, 0);
        assert_eq!(m.idempotency_fallback_count, 1);
    }

    #[tokio::test]
    async fn cache_metrics_does_not_call_stats_cache_path() {
        // Regression guard: ensure the SDK does NOT call /stats/cache (the
        // wrong path from the old placeholder implementation).
        let server = MockServer::start().await;

        // Mount a 200 handler only on the CORRECT path.
        Mock::given(method("GET"))
            .and(path("/cache/metrics"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(combined_cache_metrics_body()),
            )
            .mount(&server)
            .await;

        // /stats/cache is intentionally NOT registered — any request there
        // would receive a 404 from the mock server, which surfaces as an error.
        let client = SynapseClient::new(server.uri(), "any-key");
        let result = client.stats().cache_metrics().await;

        // If the SDK called /stats/cache instead of /cache/metrics it would
        // get a 404 (SynapseError::Http { status: 404, … }) and this assert
        // would fail.
        assert!(
            result.is_ok(),
            "SDK must call /cache/metrics, not /stats/cache; got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn cache_metrics_returns_error_on_401() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/cache/metrics"))
            .respond_with(ResponseTemplate::new(401).set_body_string("Unauthorized"))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "bad-key");
        let result = client.stats().cache_metrics().await;

        assert!(result.is_err(), "expected Err on 401, got Ok");
        match result.unwrap_err() {
            SynapseError::Api { status, .. } => {
                assert_eq!(status, 401, "expected 401 status code");
            }
            other => panic!("expected Api error, got: {other:?}"),
        }
    }
}
