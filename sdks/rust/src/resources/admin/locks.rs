use crate::client::AdminSynapseClient;
use crate::error::SynapseError;
use crate::models::LocksListResponse;

/// Admin operations for distributed locks.
///
/// # Example
///
/// ```no_run
/// use synapse_sdk::AdminSynapseClient;
///
/// # #[tokio::main]
/// # async fn main() {
/// let admin = AdminSynapseClient::builder("https://api.example.com", "admin-key").build();
/// let response = admin.locks().list().await.unwrap();
/// println!("Active locks: {}", response.total);
/// for lock in &response.active_locks {
///     println!("  {} — overdue: {}", lock.resource, lock.overdue);
/// }
/// # }
/// ```
pub struct AdminLocks<'a> {
    pub(crate) client: &'a AdminSynapseClient,
}

impl<'a> AdminLocks<'a> {
    pub fn new(client: &'a AdminSynapseClient) -> Self {
        AdminLocks { client }
    }

    /// List all currently held distributed locks.
    ///
    /// Calls `GET /admin/locks` using the admin key (`X-Admin-Key` header).
    /// Returns an empty `active_locks` list (never `null`) when nothing is locked.
    ///
    /// # Errors
    /// - [`SynapseError::Http`] — server returned a 5xx error.
    /// - [`SynapseError::Api`] — server returned a 4xx error.
    /// - [`SynapseError::Network`] — network-level failure.
    pub async fn list(&self) -> Result<LocksListResponse, SynapseError> {
        self.client.get("/admin/locks").await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn list_returns_active_locks_on_200() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/admin/locks"))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "active_locks": [
                    {
                        "resource": "settlement:42",
                        "token": "tok-abc",
                        "acquired_at": 1700000000_u64,
                        "ttl_secs": 30_u64,
                        "expected_duration_secs": 30_u64,
                        "overdue": false
                    }
                ],
                "total": 1,
                "overdue": 0
            })))
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let result = AdminLocks::new(&client).list().await;
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let resp = result.unwrap();
        assert_eq!(resp.total, 1);
        assert_eq!(resp.overdue, 0);
        assert_eq!(resp.active_locks.len(), 1);
        assert_eq!(resp.active_locks[0].resource, "settlement:42");
        assert!(!resp.active_locks[0].overdue);
    }

    #[tokio::test]
    async fn list_returns_empty_list_not_null_when_nothing_locked() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/admin/locks"))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "active_locks": [],
                "total": 0,
                "overdue": 0
            })))
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let result = AdminLocks::new(&client).list().await;
        assert!(
            result.is_ok(),
            "empty list must be Ok, not an error: {:?}",
            result
        );
        let resp = result.unwrap();
        assert_eq!(resp.total, 0);
        assert!(
            resp.active_locks.is_empty(),
            "active_locks must be an empty Vec, not null"
        );
    }

    #[tokio::test]
    async fn list_uses_admin_key_not_public_key() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/admin/locks"))
            .and(header("X-Admin-Key", "admin-test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "active_locks": [],
                "total": 0,
                "overdue": 0
            })))
            .mount(&server)
            .await;

        let client = AdminSynapseClient::builder(server.uri(), "admin-test-key").build();
        let result = AdminLocks::new(&client).list().await;
        assert!(
            result.is_ok(),
            "expected Ok with admin key, got: {:?}",
            result
        );
    }
}
