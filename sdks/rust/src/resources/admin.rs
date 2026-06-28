use crate::client::SynapseClient;
use crate::error::SynapseError;
use crate::models::BulkStatusResponse;
use serde_json::json;

/// Access admin endpoints (requires an admin API key sent via `X-API-Key`).
pub struct Admin<'a> {
    pub(crate) client: &'a SynapseClient,
}

impl<'a> Admin<'a> {
    /// Bulk-update the status of up to 500 transactions
    /// (`POST /admin/transactions/bulk-status`).
    ///
    /// The admin API key must be set on the client — not the public key.
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
    ///
    /// # Example
    ///
    /// ```no_run
    /// use synapse_sdk::SynapseClient;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// // Use the admin key, not the public key.
    /// let client = SynapseClient::new("https://api.example.com", "admin-api-key");
    ///
    /// let ids = vec![
    ///     "550e8400-e29b-41d4-a716-446655440000".to_string(),
    ///     "550e8400-e29b-41d4-a716-446655440001".to_string(),
    /// ];
    ///
    /// let resp = client.admin().bulk_update_status(ids, "completed").await.unwrap();
    /// println!("updated: {}, failed: {}", resp.updated, resp.failed);
    ///
    /// for err in &resp.errors {
    ///     eprintln!("id {} failed: {}", err.id, err.error);
    /// }
    /// # }
    /// ```
    pub async fn bulk_update_status(
        &self,
        ids: Vec<String>,
        new_status: &str,
    ) -> Result<BulkStatusResponse, SynapseError> {
        let body = json!({
            "transaction_ids": ids,
            "status": new_status,
        });
        self.client.post("/admin/transactions/bulk-status", body).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn bulk_update_status_happy_path() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/admin/transactions/bulk-status"))
            .and(header("X-API-Key", "admin-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "updated": 2,
                "failed": 0,
                "errors": []
            })))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "admin-key");
        let ids = vec![
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
            "550e8400-e29b-41d4-a716-446655440001".to_string(),
        ];
        let result = client.admin().bulk_update_status(ids, "completed").await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let resp = result.unwrap();
        assert_eq!(resp.updated, 2);
        assert_eq!(resp.failed, 0);
        assert!(resp.errors.is_empty());
    }

    #[tokio::test]
    async fn bulk_update_status_uses_admin_key_not_public_key() {
        let server = MockServer::start().await;
        // Strict match on the admin key header.
        Mock::given(method("POST"))
            .and(path("/admin/transactions/bulk-status"))
            .and(header("X-API-Key", "admin-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "updated": 1, "failed": 0, "errors": []
            })))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "admin-secret");
        let result = client
            .admin()
            .bulk_update_status(
                vec!["550e8400-e29b-41d4-a716-446655440000".to_string()],
                "failed",
            )
            .await;

        assert!(result.is_ok(), "admin key must be forwarded: {:?}", result);
    }

    #[tokio::test]
    async fn bulk_update_status_empty_ids_returns_api_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/admin/transactions/bulk-status"))
            .respond_with(ResponseTemplate::new(400).set_body_string(
                "transaction_ids must not be empty",
            ))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "admin-key");
        let result = client.admin().bulk_update_status(vec![], "completed").await;

        assert!(
            matches!(result, Err(SynapseError::Api { status: 400, .. })),
            "empty ids must return Api error 400, got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn bulk_update_status_invalid_status_returns_api_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/admin/transactions/bulk-status"))
            .respond_with(ResponseTemplate::new(422).set_body_string("invalid status 'unknown'"))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "admin-key");
        let result = client
            .admin()
            .bulk_update_status(
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
