use crate::client::SynapseClient;
use crate::error::SynapseError;
use crate::models::LocksListResponse;

pub struct Locks<'a> {
    pub(crate) client: &'a SynapseClient,
}

impl<'a> Locks<'a> {
    /// List all currently held distributed locks.
    ///
    /// Calls `GET /admin/locks` using the admin key (`X-Admin-Key` header).
    /// Returns an empty `active_locks` list (never `null`) when nothing is locked.
    ///
    /// # Errors
    /// - [`SynapseError::AdminKeyNotConfigured`] — no admin key was set on the client.
    /// - [`SynapseError::Http`] — server returned a non-success status.
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
    /// let response = client.admin().locks().list().await.unwrap();
    /// println!("Active locks: {}", response.total);
    /// for lock in &response.active_locks {
    ///     println!("  {} — overdue: {}", lock.resource, lock.overdue);
    /// }
    ///
    /// // Empty list when nothing is locked — never null.
    /// if response.active_locks.is_empty() {
    ///     println!("No locks currently held.");
    /// }
    /// # }
    /// ```
    pub async fn list(&self) -> Result<LocksListResponse, SynapseError> {
        self.client.admin_get("/admin/locks").await
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
            .and(header("X-Admin-Key", "admin-secret"))
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

        let client = SynapseClient::builder(server.uri(), "pub-key")
            .admin_key("admin-secret")
            .build();

        let result = client.admin().locks().list().await;
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
            .and(header("X-Admin-Key", "admin-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "active_locks": [],
                "total": 0,
                "overdue": 0
            })))
            .mount(&server)
            .await;

        let client = SynapseClient::builder(server.uri(), "pub-key")
            .admin_key("admin-secret")
            .build();

        let result = client.admin().locks().list().await;
        assert!(result.is_ok(), "empty list must be Ok, not an error: {:?}", result);
        let resp = result.unwrap();
        assert_eq!(resp.total, 0);
        assert!(resp.active_locks.is_empty(), "active_locks must be an empty Vec, not null");
    }

    #[tokio::test]
    async fn list_uses_admin_key_not_public_key() {
        let server = MockServer::start().await;

        // Only match requests that carry X-Admin-Key, NOT X-API-Key
        Mock::given(method("GET"))
            .and(path("/admin/locks"))
            .and(header("X-Admin-Key", "admin-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "active_locks": [],
                "total": 0,
                "overdue": 0
            })))
            .mount(&server)
            .await;

        // A client with only a public key must fail (no admin key configured)
        let public_only = SynapseClient::new(server.uri(), "pub-key");
        let err = public_only.admin().locks().list().await;
        assert!(
            matches!(err, Err(SynapseError::AdminKeyNotConfigured)),
            "expected AdminKeyNotConfigured, got: {:?}",
            err
        );

        // A client with admin key must succeed
        let admin_client = SynapseClient::builder(server.uri(), "pub-key")
            .admin_key("admin-secret")
            .build();
        let ok = admin_client.admin().locks().list().await;
        assert!(ok.is_ok(), "admin client must succeed: {:?}", ok);
    }
}
