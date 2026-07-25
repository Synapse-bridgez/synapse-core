//! Live `events` sub-command tree.
//!
//! This module contains **only** the active, user-facing WebSocket-backed
//! event-watch command.  It does *not* contain:
//!
//! - `EventsCommand` / `EventsCommand::run()` — the dead reconnect/reconnect-status
//!   pair that was never wired into `main` and was superseded by the admin
//!   event reconnection endpoints in `commands/admin.rs`.
//! - Duplicate `ReconnectResponse` / `ReconnectStatusPayload` structs — those
//!   types belong solely to the admin command module.
//!
//! The admin event-reconnection flow is intentionally implemented in
//! `commands/admin.rs` via `AdminEventsCommands`, which uses `AdminClient`
//! and is the only path that `main` dispatches to.

use anyhow::Result;
use clap::{Args, Subcommand};

/// Top-level wrapper accepted by the `synapse events` command.
#[derive(Args, Debug)]
pub struct EventsCmd {
    #[command(subcommand)]
    pub subcommand: EventsSubcommand,
}

/// Sub-commands available under `synapse events`.
#[derive(Subcommand, Debug)]
pub enum EventsSubcommand {
    /// Stream live transaction-status updates over a WebSocket connection.
    ///
    /// Connects to `<base-url>/ws` and prints every status-change event as
    /// JSON until the connection is closed or Ctrl-C is received.
    ///
    /// # Example
    ///
    /// ```text
    /// synapse events watch --base-url https://api.example.com
    /// ```
    Watch {
        /// Synapse API base URL (overrides the `SYNAPSE_BASE_URL` environment variable).
        #[arg(long, env = "SYNAPSE_BASE_URL")]
        base_url: String,

        /// API key used for the `X-API-Key` WebSocket header
        /// (overrides `SYNAPSE_API_KEY`).
        #[arg(long, env = "SYNAPSE_API_KEY")]
        api_key: String,
    },
}

impl EventsCmd {
    /// Dispatch to the selected sub-command.
    pub async fn run(&self) -> Result<()> {
        match &self.subcommand {
            EventsSubcommand::Watch { base_url, api_key } => {
                run_watch(base_url, api_key).await
            }
        }
    }
}

/// Connect to the WebSocket endpoint and stream events to stdout.
async fn run_watch(base_url: &str, api_key: &str) -> Result<()> {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    // Convert the HTTP(S) base URL to the WS(S) scheme expected by the server.
    let ws_url = base_url
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);
    let ws_url = format!("{}/ws", ws_url.trim_end_matches('/'));

    let mut request = ws_url.as_str().into_client_request()?;
    request
        .headers_mut()
        .insert("X-API-Key", api_key.parse()?);

    let (ws_stream, _) = tokio_tungstenite::connect_async(request).await?;
    let (_, mut read) = ws_stream.split();

    eprintln!("Connected. Streaming events (Ctrl-C to stop)…");

    while let Some(msg) = read.next().await {
        match msg {
            Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                println!("{text}");
            }
            Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => {
                eprintln!("Server closed the connection.");
                break;
            }
            Ok(_) => {} // ping/pong/binary — ignore
            Err(e) => {
                eprintln!("WebSocket error: {e}");
                break;
            }
        }
    }

    Ok(())
}
