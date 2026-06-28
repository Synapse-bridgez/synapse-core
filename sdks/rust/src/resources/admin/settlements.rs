use crate::client::SynapseClient;
use crate::error::SynapseError;
use crate::models::{Settlement, UpdateSettlementStatusRequest};

pub struct Settlements<'a> {
    pub(crate) client: &'a SynapseClient,
}

impl<'a> Settlements<'a> {
    /// Transition a settlement's dispute status.
    ///
    /// Calls `PATCH /admin/settlements/{id}/status` using the admin key
    /// (`X-Admin-Key` header).
    ///
    /// Allowed transitions:
    /// - `completed` → `pending_review`, `disputed`
    /// - `pending_review` → `adjusted`, `voided`, `disputed`
    /// - `disputed` → `adjusted`, `voided`, `pending_review`
    ///
    /// # Arguments
    /// * `id` — UUID of the settlement to update.
    /// * `new_status` — target status string (e.g. `"disputed"`, `"adjusted"`).
    ///
    /// # Errors
    /// - [`SynapseError::AdminKeyNotConfigured`] — no admin key was set on the client.
    /// - [`SynapseError::Http`] — server returned a non-success status (e.g. 404, 422).
    /// - [`SynapseError::Network`] — network-level failure.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use synapse_sdk::client::SynapseClient;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let client = SynapseClient::builder("https://api.example.com", "pub-key")
    ///     .admin_key("my-admin-secret")
    ///     .build();
    ///
    /// let settlement = client
    ///     .admin()
    ///     .settlements()
    ///     .update_status("550e8400-e29b-41d4-a716-446655440000", "disputed")
    ///     .await
    ///     .unwrap();
    ///
    /// println!("New status: {}", settlement.status);
    /// # }
    /// ```
    pub async fn update_status(
        &self,
        id: &str,
        new_status: &str,
    ) -> Result<Settlement, SynapseError> {
        let path = format!("/admin/settlements/{}/status", id);
        let body = UpdateSettlementStatusRequest {
            status: new_status.to_string(),
            reason: None,
            new_total: None,
            actor: None,
        };
        self.client.admin_patch(&path, &body).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn settlement_body(id: &str, status: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "asset_code": "USD",
            "total_amount": "1000.00",
            "tx_count": 10,
            "period_start": "2024-01-01T00:00:00Z",
            "period_end": "2024-01-31T23:59:59Z",
            "status": status,
            "created_at": "2024-01-15T10:00:00Z",
            "updated_at": "2024-01-15T10:05:00Z",
            "dispute_reason": null,
            "original_total_amount": null,
            "reviewed_by": null,
            "reviewed_at": null
        })
    }

    #[tokio::test]
    async fn update_status_returns_settlement_on_200() {
        let server = MockServer::start().await;
        let id = "550e8400-e29b-41d4-a716-446655440000";

        Mock::given(method("PATCH"))
            .and(path(format!("/admin/settlements/{}/status", id)))
            .and(header("X-Admin-Key", "admin-secret"))
            .and(body_json(serde_json::json!({ "status": "disputed" })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(settlement_body(id, "disputed")),
            )
            .mount(&server)
            .await;

        let client = SynapseClient::builder(server.uri(), "pub-key")
            .admin_key("admin-secret")
            .build();

        let result = client
            .admin()
            .settlements()
            .update_status(id, "disputed")
            .await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let s = result.unwrap();
        assert_eq!(s.id, id);
        assert_eq!(s.status, "disputed");
        assert_eq!(s.asset_code, "USD");
    }

    #[tokio::test]
    async fn update_status_returns_error_on_404() {
        let server = MockServer::start().await;
        let id = "00000000-0000-0000-0000-000000000000";

        Mock::given(method("PATCH"))
            .and(path(format!("/admin/settlements/{}/status", id)))
            .respond_with(ResponseTemplate::new(404).set_body_string("settlement not found"))
            .mount(&server)
            .await;

        let client = SynapseClient::builder(server.uri(), "pub-key")
            .admin_key("admin-secret")
            .build();

        let result = client
            .admin()
            .settlements()
            .update_status(id, "disputed")
            .await;

        assert!(
            matches!(result, Err(SynapseError::Api { status: 404, .. })),
            "expected Api 404, got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn update_status_uses_admin_key_not_public_key() {
        let server = MockServer::start().await;
        let id = "550e8400-e29b-41d4-a716-446655440000";

        Mock::given(method("PATCH"))
            .and(path(format!("/admin/settlements/{}/status", id)))
            .and(header("X-Admin-Key", "admin-secret"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(settlement_body(id, "disputed")),
            )
            .mount(&server)
            .await;

        // Public-key-only client must fail
        let public_only = SynapseClient::new(server.uri(), "pub-key");
        let err = public_only
            .admin()
            .settlements()
            .update_status(id, "disputed")
            .await;
        assert!(
            matches!(err, Err(SynapseError::AdminKeyNotConfigured)),
            "expected AdminKeyNotConfigured, got: {:?}",
            err
        );
    }
}
