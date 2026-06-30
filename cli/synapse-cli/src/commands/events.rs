use crate::client::ApiClient;
use crate::formatter::{print_one, OutputFormat, TableDisplay};
use anyhow::Result;
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use serde_json::json;

// ── Response types ────────────────────────────────────────────────────────────
// Mirrors the server's ReconnectionResponse / ReconnectStatus (src/handlers/reconnection.rs)
// and the SDK's ReconnectResponse (sdks/rust/src/models.rs).

/// Top-level response returned by both `POST /reconnect` and `GET /reconnect/status`.
#[derive(Debug, Deserialize, Serialize)]
pub struct ReconnectResponse {
    /// Discriminant field (`"reconnect"` or `"error"`).
    #[serde(rename = "type")]
    pub kind: String,
    /// Embedded reconnect status (present when `kind == "reconnect"`).
    pub status: Option<ReconnectStatusPayload>,
    /// Suggested backoff interval in seconds before the next attempt.
    pub backoff_seconds: Option<u64>,
    /// Whether the client must perform a full state resync after reconnecting.
    pub requires_resync: Option<bool>,
    /// Human-readable message (present when `kind == "error"`).
    pub message: Option<String>,
}

/// The inner status object embedded inside a `ReconnectResponse`.
///
/// The server serialises this with a `"status"` tag (`snake_case`), e.g.:
/// `{"status":"ready","session_id":"…"}` or `{"status":"session_expired"}`.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ReconnectStatusPayload {
    /// Client can reconnect immediately. `session_id` is the new opaque cursor.
    Ready { session_id: String },
    /// Client must wait before reconnecting (server-side rate limit).
    RetryAfter { wait_seconds: u64 },
    /// The previous session has expired; a fresh connection is needed.
    SessionExpired,
    /// The supplied token/cursor was not a valid session identifier.
    InvalidToken,
}

impl ReconnectStatusPayload {
    /// Human-readable status label for table output.
    pub fn label(&self) -> &str {
        match self {
            ReconnectStatusPayload::Ready { .. } => "ready",
            ReconnectStatusPayload::RetryAfter { .. } => "retry_after",
            ReconnectStatusPayload::SessionExpired => "session_expired",
            ReconnectStatusPayload::InvalidToken => "invalid_token",
        }
    }

    /// The session ID, if present.
    pub fn session_id(&self) -> Option<&str> {
        match self {
            ReconnectStatusPayload::Ready { session_id } => Some(session_id.as_str()),
            _ => None,
        }
    }
}

// ── TableDisplay impls ────────────────────────────────────────────────────────

impl TableDisplay for ReconnectResponse {
    fn headers() -> Vec<&'static str> {
        vec!["TYPE", "STATUS", "SESSION ID", "BACKOFF (s)", "REQUIRES RESYNC"]
    }

    fn row(&self) -> Vec<String> {
        let status_label = self
            .status
            .as_ref()
            .map(|s| s.label().to_string())
            .unwrap_or_else(|| "-".to_string());

        let session_id = self
            .status
            .as_ref()
            .and_then(|s| s.session_id())
            .unwrap_or("-")
            .to_string();

        let backoff = self
            .backoff_seconds
            .map(|b| b.to_string())
            .unwrap_or_else(|| "-".to_string());

        let resync = self
            .requires_resync
            .map(|r| r.to_string())
            .unwrap_or_else(|| "-".to_string());

        vec![self.kind.clone(), status_label, session_id, backoff, resync]
    }
}

// ── Subcommand definitions ────────────────────────────────────────────────────

#[derive(Subcommand)]
pub enum EventsCommand {
    /// Attempt to reconnect a WebSocket session (`POST /reconnect`).
    ///
    /// Sends the opaque `cursor` (session ID) from a previous connection to the
    /// server. The server validates the session and returns:
    ///   - backoff guidance (how long to wait before connecting)
    ///   - whether a full state resync is required
    ///
    /// Edge cases:
    ///   - An expired session returns status `session_expired`, not an error.
    ///   - An invalid cursor returns status `invalid_token`, not an error.
    ///
    /// Exit codes:
    ///   0 – success (including expired/invalid token responses)
    ///   1 – network or server error
    ///
    /// Example:
    ///   synapse events reconnect --cursor 550e8400-e29b-41d4-a716-446655440000
    ///   synapse events reconnect --cursor 550e8400-e29b-41d4-a716-446655440000 --json
    #[command(name = "reconnect")]
    Reconnect {
        /// Opaque session cursor (UUID) obtained from a previous reconnect-status call.
        #[arg(long, value_name = "CURSOR")]
        cursor: String,

        /// Print output as JSON instead of a table.
        #[arg(long)]
        json: bool,
    },

    /// Check reconnection status without committing an attempt (`GET /reconnect/status`).
    ///
    /// When there is no active session (no cursor supplied), the server creates a
    /// fresh session and returns `status: ready` — it never errors on a missing
    /// session. Callers should inspect the `status` field to decide how to proceed.
    ///
    /// Edge case: calling without `--cursor` is always valid; the server returns a
    /// clean `ready` response. This is the primary edge case tested by this command.
    ///
    /// Exit codes:
    ///   0 – success (including no-session case)
    ///   1 – network or server error
    ///
    /// Example:
    ///   synapse events reconnect-status
    ///   synapse events reconnect-status --cursor 550e8400-e29b-41d4-a716-446655440000
    ///   synapse events reconnect-status --json
    #[command(name = "reconnect-status")]
    ReconnectStatus {
        /// Optional opaque session cursor to query status for a specific session.
        /// Omit to get a fresh ready status (no active session required).
        #[arg(long, value_name = "CURSOR")]
        cursor: Option<String>,

        /// Print output as JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
}

// ── Runner ────────────────────────────────────────────────────────────────────

pub async fn run(cmd: EventsCommand, base_url: &str, api_key: &str) -> Result<()> {
    let client = ApiClient::new(base_url, api_key);

    match cmd {
        // ── synapse events reconnect --cursor <CURSOR> ─────────────────────
        EventsCommand::Reconnect { cursor, json } => {
            let body = json!({ "session_id": cursor });
            let response: ReconnectResponse = client.post("/reconnect", &body).await?;
            let fmt = if json { OutputFormat::Json } else { OutputFormat::Table };
            print_one(&response, fmt);
        }

        // ── synapse events reconnect-status [--cursor <CURSOR>] ───────────
        //
        // Edge case: no cursor → server always returns a clean ready response.
        // We never error here; any non-2xx is a genuine server/network fault.
        EventsCommand::ReconnectStatus { cursor, json } => {
            let fmt = if json { OutputFormat::Json } else { OutputFormat::Table };
            let response: ReconnectResponse = match cursor {
                Some(ref token) => {
                    client
                        .get_with_query("/reconnect/status", &[("token", token.as_str())])
                        .await?
                }
                None => client.get("/reconnect/status").await?,
            };
            print_one(&response, fmt);
        }
    }

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;

    fn ready_body(session_id: &str) -> String {
        format!(
            r#"{{
                "type": "reconnect",
                "status": {{"status": "ready", "session_id": "{session_id}"}},
                "backoff_seconds": 1,
                "requires_resync": true
            }}"#
        )
    }

    fn session_expired_body() -> &'static str {
        r#"{
            "type": "reconnect",
            "status": {"status": "session_expired"},
            "backoff_seconds": 0,
            "requires_resync": false
        }"#
    }

    fn error_body(msg: &str) -> String {
        format!(r#"{{"type": "error", "message": "{msg}"}}"#)
    }

    // ── reconnect-status: no active session ────────────────────────────────

    /// Edge case: calling reconnect-status without a cursor must not error.
    /// The server returns a fresh ready response; the CLI must surface it cleanly.
    #[tokio::test]
    async fn reconnect_status_no_session_returns_cleanly() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/reconnect/status")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(ready_body("new-session-id"))
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let result: Result<ReconnectResponse> = client.get("/reconnect/status").await;
        assert!(
            result.is_ok(),
            "reconnect_status with no session must not error: {:?}",
            result
        );
        let resp = result.unwrap();
        assert_eq!(resp.kind, "reconnect");
        assert_eq!(resp.backoff_seconds, Some(1));
        assert_eq!(resp.requires_resync, Some(true));
        let session_id = resp.status.as_ref().and_then(|s| s.session_id());
        assert_eq!(session_id, Some("new-session-id"));
    }

    // ── reconnect-status: with cursor ─────────────────────────────────────

    #[tokio::test]
    async fn reconnect_status_with_cursor_passes_token_query_param() {
        let cursor = "550e8400-e29b-41d4-a716-446655440000";
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/reconnect/status")
            .match_query(mockito::Matcher::UrlEncoded(
                "token".into(),
                cursor.into(),
            ))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(ready_body(cursor))
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let result: Result<ReconnectResponse> =
            client.get_with_query("/reconnect/status", &[("token", cursor)]).await;
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let resp = result.unwrap();
        assert_eq!(resp.kind, "reconnect");
    }

    // ── reconnect-status: session expired ─────────────────────────────────

    /// Expired sessions must be returned cleanly (status `session_expired`), not as an error.
    #[tokio::test]
    async fn reconnect_status_session_expired_is_not_an_error() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/reconnect/status")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(session_expired_body())
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let result: Result<ReconnectResponse> = client.get("/reconnect/status").await;
        assert!(result.is_ok(), "session_expired must not be an error: {:?}", result);
        let resp = result.unwrap();
        assert_eq!(resp.kind, "reconnect");
        let label = resp.status.as_ref().map(|s| s.label()).unwrap_or("-");
        assert_eq!(label, "session_expired");
    }

    // ── reconnect: happy path ─────────────────────────────────────────────

    #[tokio::test]
    async fn reconnect_posts_session_id_and_returns_response() {
        let cursor = "550e8400-e29b-41d4-a716-446655440000";
        let mut server = Server::new_async().await;
        server
            .mock("POST", "/reconnect")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(ready_body(cursor))
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let body = json!({ "session_id": cursor });
        let result: Result<ReconnectResponse> = client.post("/reconnect", &body).await;
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let resp = result.unwrap();
        assert_eq!(resp.backoff_seconds, Some(1));
        assert_eq!(resp.requires_resync, Some(true));
    }

    // ── reconnect: server error propagates ────────────────────────────────

    #[tokio::test]
    async fn reconnect_server_error_returns_err() {
        let mut server = Server::new_async().await;
        server
            .mock("POST", "/reconnect")
            .with_status(429)
            .with_body("too many requests")
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let body = json!({ "session_id": "some-cursor" });
        let result: Result<ReconnectResponse> = client.post("/reconnect", &body).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("429"), "error should mention 429, got: {}", err);
    }

    // ── reconnect: error type response ────────────────────────────────────

    #[tokio::test]
    async fn reconnect_error_type_response_deserialises() {
        let mut server = Server::new_async().await;
        server
            .mock("POST", "/reconnect")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(error_body("Invalid session ID format"))
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let body = json!({ "session_id": "bad-cursor" });
        let result: Result<ReconnectResponse> = client.post("/reconnect", &body).await;
        assert!(result.is_ok(), "HTTP 200 with error type must not fail: {:?}", result);
        let resp = result.unwrap();
        assert_eq!(resp.kind, "error");
        assert_eq!(
            resp.message.as_deref(),
            Some("Invalid session ID format")
        );
    }

    // ── TableDisplay ─────────────────────────────────────────────────────────

    #[test]
    fn table_display_headers() {
        let headers = ReconnectResponse::headers();
        assert!(headers.contains(&"TYPE"));
        assert!(headers.contains(&"STATUS"));
        assert!(headers.contains(&"SESSION ID"));
        assert!(headers.contains(&"BACKOFF (s)"));
        assert!(headers.contains(&"REQUIRES RESYNC"));
    }

    #[test]
    fn table_display_row_ready() {
        let resp = ReconnectResponse {
            kind: "reconnect".to_string(),
            status: Some(ReconnectStatusPayload::Ready {
                session_id: "abc-123".to_string(),
            }),
            backoff_seconds: Some(5),
            requires_resync: Some(false),
            message: None,
        };
        let row = resp.row();
        assert_eq!(row[0], "reconnect");
        assert_eq!(row[1], "ready");
        assert_eq!(row[2], "abc-123");
        assert_eq!(row[3], "5");
        assert_eq!(row[4], "false");
    }

    #[test]
    fn table_display_row_no_session() {
        let resp = ReconnectResponse {
            kind: "reconnect".to_string(),
            status: None,
            backoff_seconds: None,
            requires_resync: None,
            message: None,
        };
        let row = resp.row();
        assert_eq!(row[1], "-"); // status
        assert_eq!(row[2], "-"); // session_id
        assert_eq!(row[3], "-"); // backoff
        assert_eq!(row[4], "-"); // resync
    }

    #[test]
    fn table_display_row_session_expired() {
        let resp = ReconnectResponse {
            kind: "reconnect".to_string(),
            status: Some(ReconnectStatusPayload::SessionExpired),
            backoff_seconds: Some(0),
            requires_resync: Some(false),
            message: None,
        };
        let row = resp.row();
        assert_eq!(row[1], "session_expired");
        assert_eq!(row[2], "-"); // no session_id
    }
}
