//! Subscribe to real-time transaction events over WebSocket.
//!
//! Reads configuration from environment variables:
//!   SYNAPSE_API_URL  – base URL of the API  (default: http://localhost:3000)
//!   SYNAPSE_API_KEY  – tenant API key        (default: dev-key)
//!
//! Prints each incoming event to stdout and exits after 5 events or a
//! connection close, whichever comes first.
//!
//! Run with:
//!   cargo run --example events_subscribe
//!
//! Connection lifecycle: the WebSocket is closed cleanly on exit and no
//! dangling background task or thread is left running.

use synapse_sdk::{SynapseClient, SynapseError};

#[tokio::main]
async fn main() {
    let base_url =
        std::env::var("SYNAPSE_API_URL").unwrap_or_else(|_| "http://localhost:3000".to_string());
    let api_key = std::env::var("SYNAPSE_API_KEY").unwrap_or_else(|_| "dev-key".to_string());

    let client = SynapseClient::new(base_url, api_key);

    let mut count = 0usize;

    let result = client
        .events()
        .subscribe(
            |event| {
                println!(
                    "[{}] tx {} -> {} ({})",
                    event.timestamp,
                    event.transaction_id,
                    event.status,
                    event.message.as_deref().unwrap_or("-"),
                );
                count += 1;
                count < 5 // stop after 5 events
            },
            |err| {
                // Called for parse errors or connection errors.
                // Returning true keeps the subscription alive.
                eprintln!("error (continuing): {}", err);
                true
            },
        )
        .await;

    match result {
        Ok(()) => println!("subscription closed cleanly after {} event(s)", count),
        Err(SynapseError::Http { status: 0, body }) => {
            eprintln!("connection failed: {}", body);
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    }
}
