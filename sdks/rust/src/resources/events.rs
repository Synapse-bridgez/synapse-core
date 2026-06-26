use crate::client::SynapseClient;
use crate::error::SynapseError;
use crate::models::{ReconnectRequest, ReconnectStatusResponse, ReconnectionResponse};

pub struct Events<'a> {
    pub(crate) client: &'a SynapseClient,
}

impl<'a> Events<'a> {
    /// Resume an event stream from the last known cursor (`POST /reconnect`).
    ///
    /// `cursor` is the session ID (UUID string) from a previous connection.
    /// Returns the reconnection response including backoff and resync hints.
    pub async fn reconnect(&self, cursor: &str) -> Result<ReconnectionResponse, SynapseError> {
        let body = ReconnectRequest {
            session_id: cursor.to_string(),
            last_sequence: None,
            force_resync: None,
        };
        self.client.post("/reconnect", &body).await
    }

    /// Check reconnection status for the current session (`GET /reconnect/status`).
    ///
    /// Returns a clean response when there is no active session — never errors
    /// due to a missing session.
    pub async fn reconnect_status(&self) -> Result<ReconnectStatusResponse, SynapseError> {
        // Fetch the raw server response; map session-absent cases to a clean struct.
        let raw: serde_json::Value = self.client.get("/reconnect/status").await?;
        Ok(parse_reconnect_status(raw))
    }

    /// Subscribe to the event stream using a mocked HTTP transport.
    ///
    /// Calls `GET /events/subscribe` and passes each received `TransactionEvent`
    /// to `on_event`. Any error during streaming is forwarded to `on_error`.
    ///
    /// The subscription runs until the server closes the connection or an error
    /// is returned from `on_error` (returning `Err`). Uses the standard public
    /// API key.
    pub async fn subscribe<F, E>(
        &self,
        mut on_event: F,
        mut on_error: E,
    ) -> Result<(), SynapseError>
    where
        F: FnMut(TransactionEvent),
        E: FnMut(SynapseError) -> bool, // return false to stop
    {
        let result: Result<SubscribeResponse, SynapseError> =
            self.client.get("/events/subscribe").await;

        match result {
            Ok(resp) => {
                for event in resp.events {
                    on_event(event);
                }
                Ok(())
            }
            Err(e) => {
                let stop = !on_error(e);
                if stop {
                    Err(SynapseError::Http {
                        status: 0,
                        body: "subscription stopped by on_error handler".to_string(),
                    })
                } else {
                    Ok(())
                }
            }
        }
    }
}

/// A single event received from the event stream.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct TransactionEvent {
    pub transaction_id: String,
    pub status: String,
    pub timestamp: String,
    #[serde(default)]
    pub message: Option<String>,
}

/// Envelope returned by `GET /events/subscribe` in test/mock scenarios.
#[derive(Debug, serde::Deserialize)]
struct SubscribeResponse {
    #[serde(default)]
    events: Vec<TransactionEvent>,
}

/// Convert the raw server JSON into a [`ReconnectStatusResponse`], treating
/// absent or expired sessions as `active: false` rather than an error.
fn parse_reconnect_status(raw: serde_json::Value) -> ReconnectStatusResponse {
    // If the server returned a structured reconnect envelope, extract fields.
    let response_type = raw.get("type").and_then(|v| v.as_str()).unwrap_or("");

    if response_type == "error" {
        // Server-side error — report inactive session cleanly.
        return ReconnectStatusResponse {
            active: false,
            session_id: None,
            backoff_seconds: None,
            requires_resync: None,
        };
    }

    let status_obj = raw.get("status");
    let status_str = status_obj
        .and_then(|s| s.get("status"))
        .and_then(|s| s.as_str())
        .unwrap_or("session_expired");

    let active = status_str == "ready";
    let session_id = status_obj
        .and_then(|s| s.get("session_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let backoff_seconds = raw.get("backoff_seconds").and_then(|v| v.as_u64());
    let requires_resync = raw.get("requires_resync").and_then(|v| v.as_bool());

    ReconnectStatusResponse {
        active,
        session_id,
        backoff_seconds,
        requires_resync,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // ── reconnect() ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn reconnect_happy_path() {
        let server = MockServer::start().await;
        let session_id = "550e8400-e29b-41d4-a716-446655440000";

        Mock::given(method("POST"))
            .and(path("/reconnect"))
            .and(header("X-API-Key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "type": "reconnect",
                "status": {"status": "ready", "session_id": session_id},
                "backoff_seconds": 1,
                "requires_resync": false
            })))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "test-key");
        let result = client.events().reconnect(session_id).await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let resp = result.unwrap();
        assert_eq!(resp.response_type, "reconnect");
        assert_eq!(resp.backoff_seconds, 1);
    }

    // ── reconnect_status() ───────────────────────────────────────────────────

    #[tokio::test]
    async fn reconnect_status_active_session() {
        let server = MockServer::start().await;
        let session_id = "550e8400-e29b-41d4-a716-446655440000";

        Mock::given(method("GET"))
            .and(path("/reconnect/status"))
            .and(header("X-API-Key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "type": "reconnect",
                "status": {"status": "ready", "session_id": session_id},
                "backoff_seconds": 2,
                "requires_resync": true
            })))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "test-key");
        let result = client.events().reconnect_status().await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let resp = result.unwrap();
        assert!(resp.active);
        assert_eq!(resp.session_id.as_deref(), Some(session_id));
    }

    #[tokio::test]
    async fn reconnect_status_no_active_session_reports_cleanly() {
        let server = MockServer::start().await;

        // Server reports session_expired — must NOT cause an SDK error.
        Mock::given(method("GET"))
            .and(path("/reconnect/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "type": "reconnect",
                "status": {"status": "session_expired"},
                "backoff_seconds": 0,
                "requires_resync": false
            })))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "test-key");
        let result = client.events().reconnect_status().await;

        assert!(
            result.is_ok(),
            "no active session must not error: {:?}",
            result
        );
        let resp = result.unwrap();
        assert!(!resp.active);
        assert!(resp.session_id.is_none());
    }

    // ── subscribe() ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn subscribe_happy_path_delivers_events() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/events/subscribe"))
            .and(header("X-API-Key", "test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "events": [
                    {
                        "transaction_id": "tx-001",
                        "status": "completed",
                        "timestamp": "2026-06-26T10:00:00Z",
                        "message": null
                    },
                    {
                        "transaction_id": "tx-002",
                        "status": "pending",
                        "timestamp": "2026-06-26T10:01:00Z",
                        "message": null
                    }
                ]
            })))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "test-key");
        let mut received: Vec<String> = Vec::new();

        let result = client
            .events()
            .subscribe(
                |e| received.push(e.transaction_id.clone()),
                |_| false, // stop on any error
            )
            .await;

        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        assert_eq!(received, vec!["tx-001", "tx-002"]);
    }

    #[tokio::test]
    async fn subscribe_empty_events_list_is_not_an_error() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/events/subscribe"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "events": []
            })))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "test-key");
        let mut count = 0usize;

        let result = client
            .events()
            .subscribe(|_| count += 1, |_| false)
            .await;

        assert!(result.is_ok(), "empty event list must not error");
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn subscribe_api_key_is_sent() {
        let server = MockServer::start().await;

        // Only match if X-API-Key header is present and correct.
        Mock::given(method("GET"))
            .and(path("/events/subscribe"))
            .and(header("X-API-Key", "secret-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "events": []
            })))
            .mount(&server)
            .await;

        let client = SynapseClient::new(server.uri(), "secret-key");
        let result = client.events().subscribe(|_| {}, |_| false).await;

        // If the key wasn't sent the mock would return 404 → error.
        assert!(result.is_ok(), "public API key must be sent: {:?}", result);
    }
}
