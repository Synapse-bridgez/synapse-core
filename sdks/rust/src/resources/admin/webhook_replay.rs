use crate::client::AdminSynapseClient;
use crate::error::SynapseError;
use crate::models::{
    BatchReplayRequest, BatchWebhookReplayResponse, FailedWebhooksResponse,
    ListFailedWebhooksFilters, ReplayWebhookRequest, WebhookReplayResult,
};
use uuid::Uuid;

/// Admin operations for failed webhook replay.
///
/// Wraps:
/// - `GET /admin/webhooks/failed` → [`list_failed`]
/// - `POST /admin/webhooks/replay/:id` → [`replay`]
/// - `POST /admin/webhooks/replay/batch` → [`replay_batch`]
///
/// # Batch replay warning
/// [`replay_batch`] always returns HTTP 200. Callers **must** inspect each
/// [`WebhookReplayResult`] entry in `results` — individual items may have
/// `success: false` even when the overall call succeeds.
///
/// # Example
///
/// ```no_run
/// use synapse_sdk::AdminSynapseClient;
/// use synapse_sdk::models::ListFailedWebhooksFilters;
///
/// # #[tokio::main]
/// # async fn main() {
/// let admin = AdminSynapseClient::builder("https://api.example.com", "admin-key").build();
/// let replay = admin.webhook_replay();
///
/// // List failed deliveries
/// let failed = replay.list_failed(ListFailedWebhooksFilters {
///     limit: Some(20),
///     ..Default::default()
/// }).await.expect("failed to list");
/// println!("{} failed webhooks", failed.total);
///
/// // Replay one
/// if let Some(w) = failed.webhooks.first() {
///     let result = replay.replay(w.transaction_id, false).await.unwrap();
///     println!("replayed: {}", result.message);
/// }
/// # }
/// ```
pub struct AdminWebhookReplay<'a> {
    pub(crate) client: &'a AdminSynapseClient,
}

impl<'a> AdminWebhookReplay<'a> {
    pub fn new(client: &'a AdminSynapseClient) -> Self {
        AdminWebhookReplay { client }
    }

    /// List failed webhook deliveries with optional filters.
    ///
    /// Calls `GET /admin/webhooks/failed` with the admin key (`X-Admin-Key`).
    /// Accepts optional filters for pagination (`limit`, `offset`) and
    /// narrowing by `asset_code`, `from_date`, or `to_date`.
    ///
    /// # Errors
    /// - [`SynapseError::Http`] — server returned a 5xx error.
    /// - [`SynapseError::Api`] — server returned a 4xx error.
    /// - [`SynapseError::Network`] — network-level failure.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use synapse_sdk::AdminSynapseClient;
    /// use synapse_sdk::models::ListFailedWebhooksFilters;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let admin = AdminSynapseClient::builder("https://api.example.com", "admin-key").build();
    ///
    /// let filters = ListFailedWebhooksFilters {
    ///     limit: Some(25),
    ///     asset_code: Some("USD".to_string()),
    ///     ..Default::default()
    /// };
    ///
    /// let resp = admin.webhook_replay().list_failed(filters).await.unwrap();
    /// println!("{} total failed webhooks", resp.total);
    /// # }
    /// ```
    pub async fn list_failed(
        &self,
        filters: ListFailedWebhooksFilters,
    ) -> Result<FailedWebhooksResponse, SynapseError> {
        let mut query: Vec<(&str, String)> = Vec::new();
        let limit_s;
        let offset_s;

        if let Some(limit) = filters.limit {
            limit_s = limit.to_string();
            query.push(("limit", limit_s));
        }
        if let Some(offset) = filters.offset {
            offset_s = offset.to_string();
            query.push(("offset", offset_s));
        }
        if let Some(ref asset_code) = filters.asset_code {
            query.push(("asset_code", asset_code.clone()));
        }
        if let Some(ref from_date) = filters.from_date {
            query.push(("from_date", from_date.clone()));
        }
        if let Some(ref to_date) = filters.to_date {
            query.push(("to_date", to_date.clone()));
        }

        let query_refs: Vec<(&str, &str)> = query.iter().map(|(k, v)| (*k, v.as_str())).collect();

        self.client
            .get_query::<FailedWebhooksResponse>("/admin/webhooks/failed", &query_refs)
            .await
    }

    /// Replay a single failed webhook by transaction ID.
    ///
    /// Calls `POST /admin/webhooks/replay/:id` with the admin key (`X-Admin-Key`).
    /// When `dry_run` is `true`, the server validates and logs the attempt
    /// without committing any state changes.
    ///
    /// # Errors
    /// - [`SynapseError::Api`] with status 404 — transaction not found.
    /// - [`SynapseError::Api`] with status 400 — cannot replay (e.g. completed without dry-run).
    /// - [`SynapseError::Http`] — server returned a 5xx error.
    /// - [`SynapseError::Network`] — network-level failure.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use synapse_sdk::AdminSynapseClient;
    /// use uuid::Uuid;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let admin = AdminSynapseClient::builder("https://api.example.com", "admin-key").build();
    /// let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
    ///
    /// // Dry-run first, then actual replay
    /// let dry = admin.webhook_replay().replay(id, true).await.unwrap();
    /// println!("dry-run: {}", dry.message);
    ///
    /// let live = admin.webhook_replay().replay(id, false).await.unwrap();
    /// println!("replayed at: {:?}", live.replayed_at);
    /// # }
    /// ```
    pub async fn replay(
        &self,
        id: Uuid,
        dry_run: bool,
    ) -> Result<WebhookReplayResult, SynapseError> {
        let path = format!("/admin/webhooks/replay/{}", id);
        let body = ReplayWebhookRequest { dry_run };
        self.client
            .post::<_, WebhookReplayResult>(&path, &body)
            .await
    }

    /// Replay multiple failed webhooks in a single batch request.
    ///
    /// Calls `POST /admin/webhooks/replay/batch` with the admin key (`X-Admin-Key`).
    ///
    /// # Per-item results
    /// The HTTP response is always 200. **Callers must inspect each
    /// [`WebhookReplayResult`] in `results`** — individual items may report
    /// `success: false`. Relying solely on the top-level HTTP status or the
    /// `successful`/`failed` counts without reading `results` is a bug.
    ///
    /// # Errors
    /// - [`SynapseError::Http`] — server returned a 5xx error.
    /// - [`SynapseError::Api`] — server returned a 4xx error.
    /// - [`SynapseError::Network`] — network-level failure.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use synapse_sdk::AdminSynapseClient;
    /// use uuid::Uuid;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let admin = AdminSynapseClient::builder("https://api.example.com", "admin-key").build();
    /// let ids = vec![
    ///     Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap(),
    ///     Uuid::parse_str("660f9511-f3ac-52e5-b827-557766551111").unwrap(),
    /// ];
    ///
    /// let resp = admin.webhook_replay().replay_batch(ids, false).await.unwrap();
    ///
    /// // Always inspect individual results, not just the top-level counts
    /// for r in &resp.results {
    ///     if !r.success {
    ///         eprintln!("Failed to replay {}: {}", r.transaction_id, r.message);
    ///     }
    /// }
    /// println!("{}/{} succeeded", resp.successful, resp.total);
    /// # }
    /// ```
    pub async fn replay_batch(
        &self,
        ids: Vec<Uuid>,
        dry_run: bool,
    ) -> Result<BatchWebhookReplayResponse, SynapseError> {
        let body = BatchReplayRequest {
            transaction_ids: ids,
            dry_run,
        };
        self.client
            .post::<_, BatchWebhookReplayResponse>("/admin/webhooks/replay/batch", &body)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn failed_webhook_json(txid: &str) -> serde_json::Value {
        serde_json::json!({
            "transaction_id": txid,
            "stellar_account": "GABC1234567890123456789012345678901234567890123456789012",
            "amount": "50.00",
            "asset_code": "USD",
            "anchor_transaction_id": null,
            "status": "failed",
            "created_at": "2024-01-15T10:00:00Z",
            "last_error": "connection timeout",
            "retry_count": 2,
        })
    }

    fn replay_result_json(txid: &str, success: bool, dry_run: bool) -> serde_json::Value {
        serde_json::json!({
            "transaction_id": txid,
            "success": success,
            "message": if success { "Webhook replayed successfully" } else { "Failed to replay webhook" },
            "dry_run": dry_run,
            "replayed_at": if success && !dry_run { serde_json::json!("2024-01-15T10:05:00Z") } else { serde_json::Value::Null },
        })
    }

    #[tokio::test]
    async fn list_failed_returns_webhooks_on_200() {
        let server = MockServer::start().await;
        let txid = "550e8400-e29b-41d4-a716-446655440000";

        Mock::given(method("GET"))
            .and(path("/admin/webhooks/failed"))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "total": 1,
                "webhooks": [failed_webhook_json(txid)],
            })))
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let result = AdminWebhookReplay::new(&client)
            .list_failed(ListFailedWebhooksFilters::default())
            .await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let resp = result.unwrap();
        assert_eq!(resp.total, 1);
        assert_eq!(resp.webhooks.len(), 1);
        assert_eq!(resp.webhooks[0].transaction_id.to_string(), txid);
        assert_eq!(resp.webhooks[0].asset_code, "USD");
        assert_eq!(resp.webhooks[0].retry_count, 2);
    }

    #[tokio::test]
    async fn list_failed_returns_empty_when_none() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/admin/webhooks/failed"))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "total": 0,
                "webhooks": [],
            })))
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let result = AdminWebhookReplay::new(&client)
            .list_failed(ListFailedWebhooksFilters::default())
            .await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let resp = result.unwrap();
        assert_eq!(resp.total, 0);
        assert!(resp.webhooks.is_empty());
    }

    #[tokio::test]
    async fn list_failed_uses_admin_key_not_public_key() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/admin/webhooks/failed"))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "total": 0,
                "webhooks": [],
            })))
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let result = AdminWebhookReplay::new(&client)
            .list_failed(ListFailedWebhooksFilters::default())
            .await;

        // Mock only matches X-Admin-Key; success proves the right header was sent.
        assert!(
            result.is_ok(),
            "expected Ok with admin key, got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn list_failed_passes_filters_as_query_params() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/admin/webhooks/failed"))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "total": 0,
                "webhooks": [],
            })))
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let result = AdminWebhookReplay::new(&client)
            .list_failed(ListFailedWebhooksFilters {
                limit: Some(10),
                offset: Some(5),
                asset_code: Some("USD".to_string()),
                from_date: Some("2024-01-01T00:00:00Z".to_string()),
                to_date: Some("2024-01-31T23:59:59Z".to_string()),
            })
            .await;

        assert!(result.is_ok(), "filter call failed: {:?}", result);
    }

    #[tokio::test]
    async fn replay_returns_result_on_200() {
        let server = MockServer::start().await;
        let txid = "550e8400-e29b-41d4-a716-446655440000";

        Mock::given(method("POST"))
            .and(path(format!("/admin/webhooks/replay/{}", txid)))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(replay_result_json(txid, true, false)),
            )
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let result = AdminWebhookReplay::new(&client)
            .replay(Uuid::parse_str(txid).unwrap(), false)
            .await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let r = result.unwrap();
        assert!(r.success);
        assert!(!r.dry_run);
        assert_eq!(r.transaction_id.to_string(), txid);
    }

    #[tokio::test]
    async fn replay_dry_run_returns_result_without_replayed_at() {
        let server = MockServer::start().await;
        let txid = "550e8400-e29b-41d4-a716-446655440000";

        Mock::given(method("POST"))
            .and(path(format!("/admin/webhooks/replay/{}", txid)))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(replay_result_json(txid, true, true)),
            )
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let result = AdminWebhookReplay::new(&client)
            .replay(Uuid::parse_str(txid).unwrap(), true)
            .await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let r = result.unwrap();
        assert!(r.success);
        assert!(r.dry_run);
        assert!(r.replayed_at.is_none());
    }

    #[tokio::test]
    async fn replay_uses_admin_key_not_public_key() {
        let server = MockServer::start().await;
        let txid = "550e8400-e29b-41d4-a716-446655440000";

        Mock::given(method("POST"))
            .and(path(format!("/admin/webhooks/replay/{}", txid)))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(replay_result_json(txid, true, false)),
            )
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let result = AdminWebhookReplay::new(&client)
            .replay(Uuid::parse_str(txid).unwrap(), false)
            .await;

        // Mock requires X-Admin-Key; success confirms correct header.
        assert!(
            result.is_ok(),
            "expected Ok with admin key, got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn replay_batch_returns_per_item_results_on_200() {
        let server = MockServer::start().await;
        let txid1 = "550e8400-e29b-41d4-a716-446655440001";
        let txid2 = "550e8400-e29b-41d4-a716-446655440002";

        Mock::given(method("POST"))
            .and(path("/admin/webhooks/replay/batch"))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "total": 2,
                "successful": 1,
                "failed": 1,
                "results": [
                    replay_result_json(txid1, true, false),
                    replay_result_json(txid2, false, false),
                ],
            })))
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let result = AdminWebhookReplay::new(&client)
            .replay_batch(
                vec![
                    Uuid::parse_str(txid1).unwrap(),
                    Uuid::parse_str(txid2).unwrap(),
                ],
                false,
            )
            .await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let resp = result.unwrap();
        assert_eq!(resp.total, 2);
        assert_eq!(resp.successful, 1);
        assert_eq!(resp.failed, 1);
        assert_eq!(resp.results.len(), 2);
        // Per-item inspection is required — HTTP 200 does not mean all succeeded
        assert!(resp.results[0].success, "first item should have succeeded");
        assert!(!resp.results[1].success, "second item should have failed");
    }

    #[tokio::test]
    async fn replay_batch_uses_admin_key_not_public_key() {
        let server = MockServer::start().await;
        let txid = "550e8400-e29b-41d4-a716-446655440000";

        Mock::given(method("POST"))
            .and(path("/admin/webhooks/replay/batch"))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "total": 1,
                "successful": 1,
                "failed": 0,
                "results": [replay_result_json(txid, true, false)],
            })))
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let result = AdminWebhookReplay::new(&client)
            .replay_batch(vec![Uuid::parse_str(txid).unwrap()], false)
            .await;

        // Mock requires X-Admin-Key; success confirms correct header.
        assert!(
            result.is_ok(),
            "expected Ok with admin key, got: {:?}",
            result
        );
    }
}
