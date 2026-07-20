use crate::client::AdminSynapseClient;
use crate::error::SynapseError;
use crate::models::{AdminSettlement, UpdateSettlementStatusRequest};
use uuid::Uuid;

/// Admin operations for settlements.
///
/// Use this to manage settlement status updates. Valid status transitions depend on
/// the current status:
/// - `completed` → `pending_review`, `disputed`
/// - `pending_review` → `adjusted`, `voided`, `disputed`
/// - `disputed` → `adjusted`, `voided`, `pending_review`
///
/// Invalid transitions will return a server error with a specific validation message
/// describing the disallowed transition.
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
/// let settlements = admin.settlements();
///
/// let settlement_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
///
/// // Update status with reason
/// match settlements.update_status(
///     settlement_id,
///     "pending_review",
///     Some("Manual review requested"),
///     None,
///     None,
/// ).await {
///     Ok(updated) => println!("Settlement status updated to: {}", updated.status),
///     Err(e) => eprintln!("Error: {}", e),
/// }
/// # }
/// ```
pub struct AdminSettlements<'a> {
    pub(crate) client: &'a AdminSynapseClient,
}

impl<'a> AdminSettlements<'a> {
    /// Create a new [`AdminSettlements`] resource.
    pub fn new(client: &'a AdminSynapseClient) -> Self {
        AdminSettlements { client }
    }

    /// Update a settlement's status.
    ///
    /// Transitions between settlement statuses with optional reason and adjustment amount.
    /// The server enforces valid status transitions; attempting an invalid transition
    /// returns a 400 error with a specific message describing what transitions are allowed.
    ///
    /// # Parameters
    /// - `id`: Settlement ID to update
    /// - `new_status`: Target status string (e.g., "pending_review", "adjusted", "disputed", "voided")
    /// - `reason`: Optional reason for the status change
    /// - `new_total`: Optional new total amount, only meaningful when transitioning to "adjusted"
    /// - `actor`: Optional actor name (defaults to "admin" on server side)
    ///
    /// # Constraints
    /// - `new_status`: max 50 characters
    /// - `reason`: max 255 characters
    /// - `actor`: max 50 characters
    /// - `new_total`: valid decimal format (e.g., "1000.00")
    ///
    /// # Errors
    /// - [`SynapseError::Api`] with status 400 – invalid status transition, constraint violation,
    ///   or malformed request. The error message includes details about what went wrong.
    /// - [`SynapseError::Api`] with status 404 – settlement not found.
    /// - [`SynapseError::Http`] – server returned a 5xx error.
    /// - [`SynapseError::Network`] – network error.
    /// - [`SynapseError::Decode`] – response body is not valid JSON.
    pub async fn update_status(
        &self,
        id: Uuid,
        new_status: &str,
        reason: Option<&str>,
        new_total: Option<String>,
        actor: Option<&str>,
    ) -> Result<AdminSettlement, SynapseError> {
        let path = format!("/admin/settlements/{}/status", id);
        let req = UpdateSettlementStatusRequest {
            status: new_status.to_string(),
            reason: reason.map(|s| s.to_string()),
            new_total,
            actor: actor.map(|s| s.to_string()),
        };
        self.client.post::<_, AdminSettlement>(&path, &req).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn settlement_json(id: &str, status: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "status": status,
            "total_amount": "1000.00",
            "reason": null,
            "actor": null,
            "created_at": "2024-01-15T10:00:00Z",
            "updated_at": "2024-01-15T11:00:00Z",
        })
    }

    #[tokio::test]
    async fn update_status_returns_settlement_on_200() {
        let server = MockServer::start().await;
        let settlement_id = "550e8400-e29b-41d4-a716-446655440000";

        Mock::given(method("POST"))
            .and(path(format!("/admin/settlements/{}/status", settlement_id)))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(settlement_json(settlement_id, "pending_review")),
            )
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let settlements = AdminSettlements::new(&client);
        let result = settlements
            .update_status(
                Uuid::parse_str(settlement_id).unwrap(),
                "pending_review",
                None,
                None,
                None,
            )
            .await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let settlement = result.unwrap();
        assert_eq!(settlement.status, "pending_review");
    }

    #[tokio::test]
    async fn update_status_with_reason() {
        let server = MockServer::start().await;
        let settlement_id = "550e8400-e29b-41d4-a716-446655440000";

        Mock::given(method("POST"))
            .and(path(format!("/admin/settlements/{}/status", settlement_id)))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": settlement_id,
                "status": "disputed",
                "total_amount": "1000.00",
                "reason": "Manual dispute flag",
                "actor": "reviewer",
                "created_at": "2024-01-15T10:00:00Z",
                "updated_at": "2024-01-15T11:00:00Z",
            })))
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let settlements = AdminSettlements::new(&client);
        let result = settlements
            .update_status(
                Uuid::parse_str(settlement_id).unwrap(),
                "disputed",
                Some("Manual dispute flag"),
                None,
                Some("reviewer"),
            )
            .await;

        assert!(result.is_ok());
        let settlement = result.unwrap();
        assert_eq!(settlement.status, "disputed");
        assert_eq!(settlement.reason, Some("Manual dispute flag".to_string()));
    }

    #[tokio::test]
    async fn update_status_returns_invalid_transition_error() {
        let server = MockServer::start().await;
        let settlement_id = "550e8400-e29b-41d4-a716-446655440000";

        Mock::given(method("POST"))
            .and(path(format!("/admin/settlements/{}/status", settlement_id)))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(ResponseTemplate::new(400).set_body_string(
                "invalid status transition: cannot transition from pending to completed",
            ))
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let settlements = AdminSettlements::new(&client);
        let result = settlements
            .update_status(
                Uuid::parse_str(settlement_id).unwrap(),
                "completed",
                None,
                None,
                None,
            )
            .await;

        assert!(result.is_err());
        match result {
            Err(SynapseError::Api { status, .. }) => {
                assert_eq!(status, 400);
            }
            other => panic!("expected Api error with 400 status, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn update_status_with_new_total() {
        let server = MockServer::start().await;
        let settlement_id = "550e8400-e29b-41d4-a716-446655440000";

        Mock::given(method("POST"))
            .and(path(format!("/admin/settlements/{}/status", settlement_id)))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": settlement_id,
                "status": "adjusted",
                "total_amount": "1500.75",
                "reason": "Adjustment based on review",
                "actor": "admin",
                "created_at": "2024-01-15T10:00:00Z",
                "updated_at": "2024-01-15T12:00:00Z",
            })))
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let settlements = AdminSettlements::new(&client);
        let result = settlements
            .update_status(
                Uuid::parse_str(settlement_id).unwrap(),
                "adjusted",
                Some("Adjustment based on review"),
                Some("1500.75".to_string()),
                None,
            )
            .await;

        assert!(result.is_ok());
        let settlement = result.unwrap();
        assert_eq!(settlement.status, "adjusted");
        assert_eq!(settlement.total_amount, "1500.75");
    }

    #[tokio::test]
    async fn update_status_returns_not_found_on_404() {
        let server = MockServer::start().await;
        let settlement_id = "00000000-0000-0000-0000-000000000000";

        Mock::given(method("POST"))
            .and(path(format!("/admin/settlements/{}/status", settlement_id)))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(ResponseTemplate::new(404).set_body_string("Settlement not found"))
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let settlements = AdminSettlements::new(&client);
        let result = settlements
            .update_status(
                Uuid::parse_str(settlement_id).unwrap(),
                "pending_review",
                None,
                None,
                None,
            )
            .await;

        assert!(result.is_err());
        match result {
            Err(SynapseError::Api { status, .. }) => {
                assert_eq!(status, 404);
            }
            other => panic!("expected Api error with 404 status, got: {:?}", other),
        }
    }

    #[tokio::test]
    async fn update_status_uses_admin_key_not_public_key() {
        let server = MockServer::start().await;
        let settlement_id = "550e8400-e29b-41d4-a716-446655440000";

        Mock::given(method("POST"))
            .and(path(format!("/admin/settlements/{}/status", settlement_id)))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(settlement_json(settlement_id, "pending_review")),
            )
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let settlements = AdminSettlements::new(&client);
        let result = settlements
            .update_status(
                Uuid::parse_str(settlement_id).unwrap(),
                "pending_review",
                None,
                None,
                None,
            )
            .await;

        assert!(
            result.is_ok(),
            "expected Ok with admin key, got: {:?}",
            result
        );
    }
}
