use crate::client::SynapseClient;
use crate::error::SynapseError;
use crate::models::{ReconnectResponse, ReconnectStatusResponse};

/// Handle to the `events` resource.
pub struct Events<'a> {
    pub(crate) client: &'a SynapseClient,
}

impl<'a> Events<'a> {
    /// Reconnect to the event stream, resuming from `cursor`.
    ///
    /// Calls `POST /reconnect` with the provided session cursor so the server
    /// can resume delivery from the last acknowledged event. Uses the standard
    /// public client (`X-API-Key`).
    ///
    /// # Errors
    /// - [`SynapseError::Api`] – server returned a non-success status.
    /// - [`SynapseError::Network`] – network error before a response was received.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use synapse_sdk::SynapseClient;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let client = SynapseClient::new("https://api.example.com", "your-api-key");
    ///
    /// let resp = client
    ///     .events()
    ///     .reconnect("550e8400-e29b-41d4-a716-446655440000")
    ///     .await
    ///     .unwrap();
    ///
    /// println!("backoff: {}s", resp.backoff_seconds);
    /// # }
    /// ```
    pub async fn reconnect(&self, cursor: &str) -> Result<ReconnectResponse, SynapseError> {
        self.client
            .post_json::<_, ReconnectResponse>(
                "/reconnect",
                &serde_json::json!({ "session_id": cursor }),
            )
            .await
    }

    /// Query reconnection status without committing a reconnect attempt.
    ///
    /// Calls `GET /reconnect/status`. Useful for back-off logic: call this
    /// first to determine whether a reconnect is allowed before calling
    /// [`reconnect`](Self::reconnect).
    ///
    /// # Errors
    /// - [`SynapseError::Api`] – server returned a non-success status.
    /// - [`SynapseError::Network`] – network error before a response was received.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use synapse_sdk::SynapseClient;
    ///
    /// # #[tokio::main]
    /// # async fn main() {
    /// let client = SynapseClient::new("https://api.example.com", "your-api-key");
    ///
    /// let status = client.events().reconnect_status(None).await.unwrap();
    /// println!("status type: {}", status.status_type);
    /// # }
    /// ```
    pub async fn reconnect_status(
        &self,
        token: Option<&str>,
    ) -> Result<ReconnectStatusResponse, SynapseError> {
        let mut query: Vec<(&str, &str)> = Vec::new();
        if let Some(t) = token {
            query.push(("token", t));
        }
        self.client
            .get_query::<ReconnectStatusResponse>("/reconnect/status", &query)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn reconnect_body() -> serde_json::Value {
        serde_json::json!({
            "type": "reconnect",
            "status": { "status": "ready", "session_id": "550e8400-e29b-41d4-a716-446655440000" },
            "backoff_seconds": 1,
            "requires_resync": false
        })
    }

    fn status_body() -> serde_json::Value {
        serde_json::json!({
            "type": "reconnect",
            "status": { "status": "ready", "session_id": "550e8400-e29b-41d4-a716-446655440000" },
            "backoff_seconds": 1,
            "requires_resync": true
        })
    }

    #[tokio::test]
    async fn reconnect_returns_ok_on_200() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/reconnect"))
            .and(header("X-API-Key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(reconnect_body()))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "test-key");
        let result = client
            .events()
            .reconnect("550e8400-e29b-41d4-a716-446655440000")
            .await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        assert_eq!(result.unwrap().backoff_seconds, 1);
    }

    #[tokio::test]
    async fn reconnect_sends_public_api_key() {
        // Must use the public X-API-Key header, not an admin key.
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/reconnect"))
            .and(header("X-API-Key", "public-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(reconnect_body()))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "public-key");
        let result = client.events().reconnect("some-cursor").await;
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
    }

    #[tokio::test]
    async fn reconnect_status_returns_ok_on_200() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/reconnect/status"))
            .and(header("X-API-Key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(status_body()))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "test-key");
        let result = client.events().reconnect_status(None).await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        assert!(result.unwrap().requires_resync);
    }

    #[tokio::test]
    async fn reconnect_status_with_token_passes_query_param() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/reconnect/status"))
            .and(wiremock::matchers::query_param(
                "token",
                "550e8400-e29b-41d4-a716-446655440000",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(status_body()))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "test-key");
        let result = client
            .events()
            .reconnect_status(Some("550e8400-e29b-41d4-a716-446655440000"))
            .await;
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
    }
}
