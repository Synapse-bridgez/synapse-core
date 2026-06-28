use crate::client::AdminSynapseClient;
use crate::error::SynapseError;
use crate::models::{Settlement, UpdateSettlementStatusRequest};
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
    /// # Valid Status Transitions
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
    /// # Errors
    /// - [`SynapseError::Api`] with status 400 – invalid status transition, constraint violation,
    ///   or malformed request. The error message includes details about what went wrong.
    /// - [`SynapseError::Api`] with status 404 – settlement not found.
    /// - [`SynapseError::Http`] – server returned a 5xx error.
    /// - [`SynapseError::Network`] – network error.
    /// - [`SynapseError::Decode`] – response body is not valid JSON.
    ///
    /// # Example: Simple Status Change
    ///
    /// ```no_run
    /// use synapse_sdk::AdminSynapseClient;
    /// use synapse_sdk::SynapseError;
    /// use uuid::Uuid;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let admin = AdminSynapseClient::builder("https://api.example.com", "admin-key").build();
    /// let settlements = admin.settlements();
    ///
    /// let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
    ///
    /// match settlements.update_status(id, "pending_review", None, None, None).await {
    ///     Ok(settlement) => {
    ///         println!("✓ Settlement {} is now {}", settlement.id, settlement.status);
    ///     }
    ///     Err(e) => eprintln!("✗ Update failed: {}", e),
    /// }
    /// # }
    /// ```
    ///
    /// # Example: Status Change with Reason and Validation Error
    ///
    /// ```no_run
    /// use synapse_sdk::AdminSynapseClient;
    /// use synapse_sdk::SynapseError;
    /// use uuid::Uuid;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let admin = AdminSynapseClient::builder("https://api.example.com", "admin-key").build();
    /// let settlements = admin.settlements();
    ///
    /// let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
    ///
    /// // Attempt an invalid transition
    /// match settlements.update_status(
    ///     id,
    ///     "completed",  // Invalid: can only transition FROM completed, not TO it
    ///     Some("Trying to mark as completed"),
    ///     None,
    ///     None,
    /// ).await {
    ///     Ok(_) => println!("Unexpectedly succeeded"),
    ///     Err(SynapseError::Api { status, message }) if status == 400 => {
    ///         // Server returns specific validation error
    ///         eprintln!("✗ Invalid transition: {}", message);
    ///     }
    ///     Err(e) => eprintln!("✗ Other error: {}", e),
    /// }
    /// # }
    /// ```
    ///
    /// # Example: Adjustment with New Total
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
    /// let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
    ///
    /// match settlements.update_status(
    ///     id,
    ///     "adjusted",
    ///     Some("Manually adjusted based on discrepancy review"),
    ///     Some("5000.50".to_string()),  // New total amount
    ///     Some("reviewer_name"),
    /// ).await {
    ///     Ok(updated) => {
    ///         println!("✓ Adjusted to {} (amount: {})",
    ///                  updated.status, updated.total_amount);
    ///     }
    ///     Err(e) => eprintln!("✗ Adjustment failed: {}", e),
    /// }
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
        id: Uuid,
        new_status: &str,
        reason: Option<&str>,
        new_total: Option<String>,
        actor: Option<&str>,
    ) -> Result<Settlement, SynapseError> {
        let path = format!("/admin/settlements/{}/status", id);
        let req = UpdateSettlementStatusRequest {
            status: new_status.to_string(),
            reason: reason.map(|s| s.to_string()),
            new_total,
            actor: actor.map(|s| s.to_string()),
        };
        self.client
            .post::<_, Settlement>(&path, &req)
            .await
    }
}

impl<'a> AdminSettlements<'a> {
    /// Create a new [`AdminSettlements`] resource.
    pub fn new(client: &'a AdminSynapseClient) -> Self {
        AdminSettlements { client }
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
        let settlement_id = "550e8400-e29b-41d4-a716-446655440000";

        Mock::given(method("POST"))
            .and(path(format!("/admin/settlements/{}/status", settlement_id)))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(settlement_json(settlement_id, "pending_review")),
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
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id": settlement_id,
                    "status": "disputed",
                    "total_amount": "1000.00",
                    "reason": "Manual dispute flag",
                    "actor": "reviewer",
                    "created_at": "2024-01-15T10:00:00Z",
                    "updated_at": "2024-01-15T11:00:00Z",
                })),
            )
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
            .respond_with(
                ResponseTemplate::new(400).set_body_string(
                    "invalid status transition: cannot transition from pending to completed",
                ),
            )
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
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id": settlement_id,
                    "status": "adjusted",
                    "total_amount": "1500.75",
                    "reason": "Adjustment based on review",
                    "actor": "admin",
                    "created_at": "2024-01-15T10:00:00Z",
                    "updated_at": "2024-01-15T12:00:00Z",
                })),
            )
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
            .respond_with(
                ResponseTemplate::new(404).set_body_string("Settlement not found"),
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
