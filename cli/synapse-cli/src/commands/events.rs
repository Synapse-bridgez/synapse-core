use anyhow::Result;
use chrono::{DateTime, Utc};
use clap::{Args, Subcommand};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use uuid::Uuid;

use crate::formatter::{Formatter, OutputFormat};

/// A real-time transaction status update pushed by the server.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TransactionStatusUpdate {
    pub transaction_id: Uuid,
    pub tenant_id: Uuid,
    pub status: String,
    pub timestamp: DateTime<Utc>,
    pub message: Option<String>,
}

#[derive(Args)]
pub struct EventsCmd {
    #[command(subcommand)]
    pub command: EventsSubcommand,
}

#[derive(Subcommand)]
pub enum EventsSubcommand {
    /// Stream real-time transaction status events from the server.
    ///
    /// Connects to the server WebSocket endpoint (GET /ws?token=<TOKEN>) and
    /// prints each incoming TransactionStatusUpdate. Runs until the server
    /// closes the connection or Ctrl-C is pressed.
    ///
    /// Connection lifecycle: a WebSocket Close frame is sent on exit — no
    /// background task or thread is left running.
    Watch {
        /// API token forwarded as the `?token=` WebSocket query parameter.
        /// Required. Can also be set via the SYNAPSE_API_KEY environment variable.
        #[arg(long, env = "SYNAPSE_API_KEY", default_value = "")]
        token: String,

        /// Output format. Optional, default: table.
        /// Use `json` for machine-readable output.
        #[arg(long, default_value = "table")]
        format: String,
    },

    /// Attempt to reconnect a previous WebSocket session (POST /reconnect).
    ///
    /// Sends the session ID from a previous connection to the server, which
    /// validates the session and returns:
    ///   - status: ready | retry_after | session_expired | invalid_token
    ///   - backoff_seconds: how long to wait before the next attempt
    ///   - requires_resync: whether to perform a full state resync on connect
    ///
    /// Required flags:
    ///   --session-id  UUID of the session to resume (from reconnect-status)
    ///
    /// Optional flags:
    ///   --last-sequence  Last event sequence number received (enables gap
    ///                    detection; omit to force full resync)
    ///   --force-resync   Always request a full state resync (default: false)
    ///   --format         Output format: table (default) or json
    ///
    /// Exit codes:
    ///   0  Session accepted (status may still be retry_after or expired)
    ///   1  Network error or server returned a non-200 response
    #[command(name = "reconnect")]
    Reconnect {
        /// UUID of the session to resume.
        /// Required. Obtain from a prior `reconnect-status` or `watch` run.
        #[arg(long, value_name = "UUID")]
        session_id: String,

        /// Last event sequence number the client received. Optional.
        /// When provided, the server can detect gaps and set requires_resync
        /// accordingly. Omit to always request a full resync.
        #[arg(long, value_name = "N")]
        last_sequence: Option<i64>,

        /// Force a full state resync regardless of sequence position. Optional, default: false.
        #[arg(long, default_value_t = false)]
        force_resync: bool,

        /// Output format. Optional, default: table.
        /// Use `json` for machine-readable output or scripting.
        #[arg(long, default_value = "table")]
        format: String,
    },

    /// Check reconnection status before committing an attempt (GET /reconnect/status).
    ///
    /// Call this before `reconnect` to discover whether the server will accept
    /// a reconnection right now. The server never returns an error for a missing
    /// session — it always returns a status value that tells you how to proceed.
    ///
    /// Required flags:  none (all flags are optional)
    ///
    /// Optional flags:
    ///   --token          Session token / UUID from a previous connection.
    ///                    When omitted the server creates a fresh session and
    ///                    returns status=ready with a new session_id.
    ///   --last-sequence  Last event sequence number received (used by the server
    ///                    to compute whether requires_resync should be true)
    ///   --format         Output format: table (default) or json
    ///
    /// Status values in the response:
    ///   ready          — reconnect immediately; session_id is valid
    ///   retry_after    — rate-limited; wait `backoff_seconds` before retrying
    ///   session_expired — session is no longer valid; start a new connection
    ///   invalid_token  — token could not be parsed; check the value passed
    ///
    /// Exit codes:
    ///   0  Status retrieved successfully
    ///   1  Network error or server returned a non-200 response
    #[command(name = "reconnect-status")]
    ReconnectStatus {
        /// Session token UUID from a previous connection. Optional.
        /// When omitted the server allocates a fresh session (status=ready).
        #[arg(long, value_name = "UUID")]
        token: Option<String>,

        /// Last event sequence number received. Optional.
        /// Passed as the `last_sequence` query parameter so the server can
        /// advise whether a full resync is needed.
        #[arg(long, value_name = "N")]
        last_sequence: Option<i64>,

        /// Output format. Optional, default: table.
        /// Use `json` for machine-readable output or scripting.
        #[arg(long, default_value = "table")]
        format: String,
    },
}

/// Subscribe to real-time events by driving the WebSocket inline.
///
/// Calls `on_event` for each parsed [`TransactionStatusUpdate`] and `on_error`
/// for any parse / connection error. Returns when the server closes the
/// connection, `on_event` returns `false`, or `on_error` returns `false`.
///
/// **Connection lifecycle**: a Close frame is sent before returning; no
/// background task is left running.
pub async fn subscribe<FE, FErr>(
    base_url: &str,
    token: &str,
    mut on_event: FE,
    mut on_error: FErr,
) -> Result<()>
where
    FE: FnMut(TransactionStatusUpdate) -> bool,
    FErr: FnMut(anyhow::Error) -> bool,
{
    // Convert http(s) base_url to ws(s) and append the path + token.
    let ws_url = base_url
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);
    let ws_url = format!("{}/ws?token={}", ws_url.trim_end_matches('/'), token);

    let (ws_stream, _) = connect_async(&ws_url)
        .await
        .map_err(|e| anyhow::anyhow!("WebSocket handshake failed: {}", e))?;

    let (mut write, mut read) = ws_stream.split();

    loop {
        match read.next().await {
            Some(Ok(Message::Text(text))) => {
                match serde_json::from_str::<TransactionStatusUpdate>(&text) {
                    Ok(event) => {
                        if !on_event(event) {
                            break;
                        }
                    }
                    Err(e) => {
                        if !on_error(anyhow::anyhow!("parse error: {}", e)) {
                            break;
                        }
                    }
                }
            }
            Some(Ok(Message::Close(_))) | None => break,
            Some(Ok(_)) => {} // ignore ping/pong/binary frames
            Some(Err(e)) => {
                if !on_error(anyhow::anyhow!("connection error: {}", e)) {
                    break;
                }
            }
        }
    }

    // Send close frame — ensures the socket is shut down cleanly and no
    // background task is left running.
    let _ = write.send(Message::Close(None)).await;
    let _ = write.close().await;

    Ok(())
}

/// Handle the `events watch` subcommand end-to-end.
pub async fn handle_events(cmd: EventsCmd, base_url: &str) -> Result<()> {
    match cmd.command {
        EventsSubcommand::Watch { token, format } => {
            let fmt = OutputFormat::from_str(&format);

            subscribe(
                base_url,
                &token,
                |event| {
                    match Formatter::format_json_output(&event, fmt) {
                        Ok(output) => println!("{}", output),
                        Err(e) => eprintln!("format error: {}", e),
                    }
                    true // keep streaming
                },
                |err| {
                    eprintln!("error: {}", err);
                    true // keep streaming on transient errors
                },
            )
            .await
        }

        EventsSubcommand::Reconnect {
            session_id,
            last_sequence,
            force_resync,
            format,
        } => {
            let fmt = OutputFormat::from_str(&format);
            let url = format!("{}/reconnect", base_url.trim_end_matches('/'));
            let mut body = serde_json::json!({ "session_id": session_id });
            if let Some(seq) = last_sequence {
                body["last_sequence"] = serde_json::json!(seq);
            }
            if force_resync {
                body["force_resync"] = serde_json::json!(true);
            }
            let resp: serde_json::Value = reqwest::Client::new()
                .post(&url)
                .json(&body)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("request failed: {}", e))?
                .json()
                .await
                .map_err(|e| anyhow::anyhow!("failed to parse response: {}", e))?;
            println!("{}", Formatter::format_json_output(&resp, fmt)?);
            Ok(())
        }

        EventsSubcommand::ReconnectStatus {
            token,
            last_sequence,
            format,
        } => {
            let fmt = OutputFormat::from_str(&format);
            let base = base_url.trim_end_matches('/');
            let client = reqwest::Client::new();
            let mut req = client.get(format!("{}/reconnect/status", base));
            if let Some(ref t) = token {
                req = req.query(&[("token", t.as_str())]);
            }
            if let Some(seq) = last_sequence {
                req = req.query(&[("last_sequence", seq.to_string().as_str())]);
            }
            let resp: serde_json::Value = req
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("request failed: {}", e))?
                .json()
                .await
                .map_err(|e| anyhow::anyhow!("failed to parse response: {}", e))?;
            println!("{}", Formatter::format_json_output(&resp, fmt)?);
            Ok(())
        }
    }
}
