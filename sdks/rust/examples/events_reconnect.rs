//! Demonstrate `events.reconnect_status()` and `events.reconnect()`.
//!
//! `reconnect_status()` must return cleanly when there is no active session —
//! it must not error. The session_id from the status response is then used
//! to call `reconnect()`.
//!
//! Run with:
//!   cargo run --example events_reconnect

use synapse_sdk::SynapseClient;

#[tokio::main]
async fn main() {
    let base_url =
        std::env::var("SYNAPSE_API_URL").unwrap_or_else(|_| "http://localhost:3000".to_string());
    let api_key = std::env::var("SYNAPSE_API_KEY").unwrap_or_else(|_| "dev-key".to_string());
    let client = SynapseClient::new(base_url, api_key);

    // reconnect_status() with no active session must not error.
    let status = match client.events().reconnect_status(None).await {
        Ok(s) => {
            println!("reconnect_status (no session): type={}", s.kind);
            s
        }
        Err(e) => {
            eprintln!("reconnect_status error: {e}");
            return;
        }
    };

    // Extract session_id if the server returned one.
    let session_id = status
        .status
        .as_ref()
        .and_then(|s| match s {
            synapse_sdk::models::ReconnectStatus::Ready { session_id } => {
                Some(session_id.clone())
            }
            _ => None,
        });

    if let Some(cursor) = session_id {
        println!("Got session_id: {cursor}");

        // reconnect() with the cursor from the previous status call.
        match client.events().reconnect(cursor).await {
            Ok(r) => println!(
                "reconnect: backoff={}s resync={:?}",
                r.backoff_seconds.unwrap_or(0),
                r.requires_resync
            ),
            Err(e) => eprintln!("reconnect error: {e}"),
        }
    }
}
