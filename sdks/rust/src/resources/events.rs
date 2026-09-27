use crate::client::SynapseClient;
use crate::error::SynapseError;
use crate::models::ReconnectResponse;
use serde_json::json;

/// Access the reconnect/events endpoints.
pub struct Events<'a> {
    pub(crate) client: &'a SynapseClient,
}

impl<'a> Events<'a> {
    /// Attempt to reconnect a WebSocket session (`POST /reconnect`).
    ///
    /// Pass the opaque `cursor` (session ID) from a previous connection. The
    /// server validates the session and returns backoff guidance and whether a
    /// full state resync is required.
    ///
    /// # Errors
    /// - [`SynapseError::Api`] – server returned a non-success HTTP status.
    /// - [`SynapseError::Network`] – transport/network failure.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use synapse_sdk::SynapseClient;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let client = SynapseClient::new("https://api.example.com", "key");
    ///
    /// // cursor is the session_id obtained from a previous reconnect_status() call
    /// let cursor = "550e8400-e29b-41d4-a716-446655440000".to_string();
    /// let resp = client.events().reconnect(cursor).await.unwrap();
    /// println!("backoff: {:?}s", resp.backoff_seconds);
    /// println!("requires_resync: {:?}", resp.requires_resync);
    /// # }
    /// ```
    pub async fn reconnect(&self, cursor: String) -> Result<ReconnectResponse, SynapseError> {
        let body = json!({ "session_id": cursor });
        self.client.post("/reconnect", body).await
    }

    /// Check reconnection status without committing an attempt (`GET /reconnect/status`).
    ///
    /// When there is no active session (no `cursor` / token), the server
    /// returns a fresh `Ready` status — it never errors on a missing session.
    /// Callers should always check the `kind` field to determine how to proceed.
    ///
    /// # Errors
    /// - [`SynapseError::Api`] – server returned a non-success HTTP status.
    /// - [`SynapseError::Network`] – transport/network failure.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use synapse_sdk::SynapseClient;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let client = SynapseClient::new("https://api.example.com", "key");
    ///
    /// // No active session — must return cleanly, not error.
    /// let status = client.events().reconnect_status(None).await.unwrap();
    /// println!("type: {}", status.kind);
    ///
    /// // With an existing session cursor:
    /// let cursor = "550e8400-e29b-41d4-a716-446655440000";
    /// let status = client.events().reconnect_status(Some(cursor)).await.unwrap();
    /// println!("backoff: {:?}s", status.backoff_seconds);
    /// # }
    /// ```
    pub async fn reconnect_status(
        &self,
        cursor: Option<&str>,
    ) -> Result<ReconnectResponse, SynapseError> {
        match cursor {
            Some(token) => {
                self.client
                    .get_query("/reconnect/status", &[("token", token)])
                    .await
            }
            None => self.client.get("/reconnect/status").await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn ready_response(session_id: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "reconnect",
            "status": { "status": "ready", "session_id": session_id },
            "backoff_seconds": 1,
            "requires_resync": true
        })
    }

    #[tokio::test]
    async fn reconnect_status_no_session_returns_cleanly() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/reconnect/status"))
            .and(header("X-API-Key", "k"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(ready_response("new-session-id")),
            )
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "k");
        // No active session — must not error.
        let result = client.events().reconnect_status(None).await;
        assert!(
            result.is_ok(),
            "reconnect_status with no session must not error: {:?}",
            result
        );
        let resp = result.unwrap();
        assert_eq!(resp.kind, "reconnect");
    }

    #[tokio::test]
    async fn reconnect_posts_session_id_and_returns_response() {
        let server = MockServer::start().await;
        let session = "550e8400-e29b-41d4-a716-446655440000";
        Mock::given(method("POST"))
            .and(path("/reconnect"))
            .and(header("X-API-Key", "k"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ready_response(session)))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "k");
        let result = client.events().reconnect(session.to_string()).await;
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let resp = result.unwrap();
        assert_eq!(resp.backoff_seconds, Some(1));
        assert_eq!(resp.requires_resync, Some(true));
    }
}
