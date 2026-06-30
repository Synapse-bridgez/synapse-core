use crate::client::AdminSynapseClient;
use crate::error::SynapseError;
use crate::models::{DlqListResponse, RequeueResponse};
use uuid::Uuid;

/// Admin operations for the dead-letter queue (DLQ).
///
/// Wraps `GET /dlq` and `POST /dlq/:id/requeue`. Use this to inspect failed
/// transactions and replay them through the processor.
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
/// let dlq = admin.dlq();
///
/// // List all DLQ entries
/// let entries = dlq.list().await.expect("failed to list DLQ");
/// println!("DLQ entries: {}", entries.count);
///
/// // Requeue a specific entry
/// if let Some(entry) = entries.dlq_entries.first() {
///     match dlq.requeue(entry.id).await {
///         Ok(resp) => println!("Requeued: {}", resp.message),
///         Err(e) => eprintln!("Error: {}", e),
///     }
/// }
/// # }
/// ```
pub struct AdminDlq<'a> {
    pub(crate) client: &'a AdminSynapseClient,
}

impl<'a> AdminDlq<'a> {
    pub fn new(client: &'a AdminSynapseClient) -> Self {
        AdminDlq { client }
    }

    /// List all dead-letter queue entries.
    ///
    /// Calls `GET /dlq` with the admin key (`X-Admin-Key` header).
    /// Returns up to 100 entries ordered by `moved_to_dlq_at` descending.
    /// An empty `dlq_entries` list (never `null`) means the DLQ is clear.
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
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let admin = AdminSynapseClient::builder("https://api.example.com", "admin-key").build();
    ///
    /// match admin.dlq().list().await {
    ///     Ok(resp) => {
    ///         println!("{} entries in DLQ", resp.count);
    ///         for e in &resp.dlq_entries {
    ///             println!("  {} — {}", e.id, e.error_reason);
    ///         }
    ///     }
    ///     Err(e) => eprintln!("Failed to list DLQ: {}", e),
    /// }
    /// # }
    /// ```
    pub async fn list(&self) -> Result<DlqListResponse, SynapseError> {
        self.client.get::<DlqListResponse>("/dlq").await
    }

    /// Requeue a specific dead-letter queue entry by its ID.
    ///
    /// Calls `POST /dlq/:id/requeue` with the admin key (`X-Admin-Key` header).
    /// The server re-submits the failed transaction to the processor.
    ///
    /// # Edge case
    /// If the entry no longer exists in the DLQ (e.g. it was already requeued
    /// or manually removed), the server returns 404 and this method surfaces it
    /// as [`SynapseError::NotFound`] — never a silent success or a generic 500.
    ///
    /// # Errors
    /// - [`SynapseError::NotFound`] — the DLQ ID does not exist.
    /// - [`SynapseError::Http`] — server returned a 5xx error.
    /// - [`SynapseError::Api`] — server returned another 4xx error.
    /// - [`SynapseError::Network`] — network-level failure.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use synapse_sdk::{AdminSynapseClient, SynapseError};
    /// use uuid::Uuid;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let admin = AdminSynapseClient::builder("https://api.example.com", "admin-key").build();
    /// let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
    ///
    /// match admin.dlq().requeue(id).await {
    ///     Ok(resp) => println!("Requeued {}: {}", resp.dlq_id, resp.message),
    ///     Err(SynapseError::NotFound(msg)) => eprintln!("DLQ entry not found: {}", msg),
    ///     Err(e) => eprintln!("Error: {}", e),
    /// }
    /// # }
    /// ```
    pub async fn requeue(&self, id: Uuid) -> Result<RequeueResponse, SynapseError> {
        let path = format!("/dlq/{}/requeue", id);
        let empty = serde_json::json!({});
        match self.client.post::<_, RequeueResponse>(&path, &empty).await {
            Err(SynapseError::Api { status: 404, message }) => {
                Err(SynapseError::NotFound(message))
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn dlq_entry_json(id: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "transaction_id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
            "stellar_account": "GABC1234567890123456789012345678901234567890123456789012",
            "amount": "100.00",
            "asset_code": "USD",
            "anchor_transaction_id": null,
            "error_reason": "Payment failed: insufficient funds",
            "stack_trace": null,
            "retry_count": 3,
            "original_created_at": "2024-01-15T10:00:00Z",
            "moved_to_dlq_at": "2024-01-15T10:05:00Z",
            "last_retry_at": "2024-01-15T10:04:00Z",
        })
    }

    #[tokio::test]
    async fn list_returns_dlq_entries_on_200() {
        let server = MockServer::start().await;
        let entry_id = "550e8400-e29b-41d4-a716-446655440000";

        Mock::given(method("GET"))
            .and(path("/dlq"))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "dlq_entries": [dlq_entry_json(entry_id)],
                "count": 1,
            })))
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let result = AdminDlq::new(&client).list().await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let resp = result.unwrap();
        assert_eq!(resp.count, 1);
        assert_eq!(resp.dlq_entries.len(), 1);
        assert_eq!(resp.dlq_entries[0].id.to_string(), entry_id);
        assert_eq!(resp.dlq_entries[0].amount, "100.00");
        assert_eq!(resp.dlq_entries[0].asset_code, "USD");
        assert_eq!(resp.dlq_entries[0].retry_count, 3);
    }

    #[tokio::test]
    async fn list_returns_empty_list_when_dlq_is_clear() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/dlq"))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "dlq_entries": [],
                "count": 0,
            })))
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let result = AdminDlq::new(&client).list().await;

        assert!(result.is_ok(), "empty DLQ must be Ok, not an error: {:?}", result);
        let resp = result.unwrap();
        assert_eq!(resp.count, 0);
        assert!(resp.dlq_entries.is_empty(), "dlq_entries must be an empty Vec, not null");
    }

    #[tokio::test]
    async fn list_uses_admin_key_not_public_key() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/dlq"))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "dlq_entries": [],
                "count": 0,
            })))
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let result = AdminDlq::new(&client).list().await;

        // If the test passes the mock matched, meaning X-Admin-Key was sent, not X-API-Key.
        assert!(result.is_ok(), "expected Ok with admin key, got: {:?}", result);
    }

    #[tokio::test]
    async fn requeue_returns_success_on_200() {
        let server = MockServer::start().await;
        let entry_id = "550e8400-e29b-41d4-a716-446655440000";

        Mock::given(method("POST"))
            .and(path(format!("/dlq/{}/requeue", entry_id)))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "message": "DLQ entry requeued successfully",
                "dlq_id": entry_id,
            })))
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let result = AdminDlq::new(&client)
            .requeue(Uuid::parse_str(entry_id).unwrap())
            .await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let resp = result.unwrap();
        assert_eq!(resp.message, "DLQ entry requeued successfully");
        assert_eq!(resp.dlq_id.to_string(), entry_id);
    }

    #[tokio::test]
    async fn requeue_uses_admin_key_not_public_key() {
        let server = MockServer::start().await;
        let entry_id = "550e8400-e29b-41d4-a716-446655440000";

        Mock::given(method("POST"))
            .and(path(format!("/dlq/{}/requeue", entry_id)))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "message": "DLQ entry requeued successfully",
                "dlq_id": entry_id,
            })))
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let result = AdminDlq::new(&client)
            .requeue(Uuid::parse_str(entry_id).unwrap())
            .await;

        // Mock only matches X-Admin-Key header; success proves the right key was sent.
        assert!(result.is_ok(), "expected Ok with admin key, got: {:?}", result);
    }

    #[tokio::test]
    async fn requeue_missing_id_returns_not_found() {
        let server = MockServer::start().await;
        let missing_id = "00000000-0000-0000-0000-000000000000";

        Mock::given(method("POST"))
            .and(path(format!("/dlq/{}/requeue", missing_id)))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(
                ResponseTemplate::new(404).set_body_string("DLQ entry not found"),
            )
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let result = AdminDlq::new(&client)
            .requeue(Uuid::parse_str(missing_id).unwrap())
            .await;

        assert!(result.is_err(), "expected Err for missing DLQ ID");
        match result {
            Err(SynapseError::NotFound(msg)) => {
                assert!(
                    !msg.is_empty(),
                    "NotFound message must be non-empty"
                );
            }
            other => panic!("expected SynapseError::NotFound, got: {:?}", other),
        }
    }
}
