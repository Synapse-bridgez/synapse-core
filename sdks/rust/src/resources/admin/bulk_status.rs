use crate::client::AdminSynapseClient;
use crate::error::SynapseError;
use crate::models::BulkStatusResponse;
use serde_json::json;

/// Admin operations for bulk transaction status updates.
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
/// let ids = vec![
///     "550e8400-e29b-41d4-a716-446655440000".to_string(),
///     "550e8400-e29b-41d4-a716-446655440001".to_string(),
/// ];
///
/// let resp = admin.bulk_status().update(ids, "completed").await.unwrap();
/// println!("updated: {}, failed: {}", resp.updated, resp.failed);
///
/// for err in &resp.errors {
///     eprintln!("id {} failed: {}", err.id, err.error);
/// }
/// # }
/// ```
pub struct AdminBulkStatus<'a> {
    pub(crate) client: &'a AdminSynapseClient,
}

impl<'a> AdminBulkStatus<'a> {
    pub fn new(client: &'a AdminSynapseClient) -> Self {
        AdminBulkStatus { client }
    }

    /// Bulk-update the status of up to 500 transactions
    /// (`POST /admin/transactions/bulk-status`).
    ///
    /// `ids` must be non-empty and contain at most 500 UUIDs.
    /// `new_status` must be one of `"pending"`, `"processing"`, `"completed"`,
    /// or `"failed"`; any other value returns [`SynapseError::Api`] (HTTP 422).
    ///
    /// Partial success is normal: the server processes each ID independently.
    /// Check [`BulkStatusResponse::failed`] and [`BulkStatusResponse::errors`]
    /// for per-item failures even when the call returns `Ok`.
    ///
    /// # Errors
    /// - [`SynapseError::Api`] – empty `ids`, over-limit, invalid status, or
    ///   other non-success HTTP response.
    /// - [`SynapseError::Network`] – transport/network failure.
    pub async fn update(
        &self,
        ids: Vec<String>,
        new_status: &str,
    ) -> Result<BulkStatusResponse, SynapseError> {
        let body = json!({
            "transaction_ids": ids,
            "status": new_status,
        });
        self.client
            .post("/admin/transactions/bulk-status", &body)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn update_happy_path() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/admin/transactions/bulk-status"))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "updated": 2,
                "failed": 0,
                "errors": []
            })))
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let ids = vec![
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
            "550e8400-e29b-41d4-a716-446655440001".to_string(),
        ];
        let result = AdminBulkStatus::new(&client).update(ids, "completed").await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let resp = result.unwrap();
        assert_eq!(resp.updated, 2);
        assert_eq!(resp.failed, 0);
        assert!(resp.errors.is_empty());
    }

    #[tokio::test]
    async fn update_uses_admin_key_not_public_key() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/admin/transactions/bulk-status"))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "updated": 1, "failed": 0, "errors": []
            })))
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let result = AdminBulkStatus::new(&client)
            .update(
                vec!["550e8400-e29b-41d4-a716-446655440000".to_string()],
                "failed",
            )
            .await;

        assert!(result.is_ok(), "admin key must be forwarded: {:?}", result);
    }

    #[tokio::test]
    async fn update_empty_ids_returns_api_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/admin/transactions/bulk-status"))
            .respond_with(
                ResponseTemplate::new(400).set_body_string("transaction_ids must not be empty"),
            )
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let result = AdminBulkStatus::new(&client)
            .update(vec![], "completed")
            .await;

        assert!(
            matches!(result, Err(SynapseError::Api { status: 400, .. })),
            "empty ids must return Api error 400, got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn update_invalid_status_returns_api_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/admin/transactions/bulk-status"))
            .respond_with(ResponseTemplate::new(422).set_body_string("invalid status 'unknown'"))
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let result = AdminBulkStatus::new(&client)
            .update(
                vec!["550e8400-e29b-41d4-a716-446655440000".to_string()],
                "unknown",
            )
            .await;

        assert!(
            matches!(result, Err(SynapseError::Api { status: 422, .. })),
            "invalid status must return Api error 422, got: {:?}",
            result
        );
    }
}
