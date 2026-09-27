//! List transactions page-by-page and print each row.
//!
//! Reads configuration from environment variables:
//!   SYNAPSE_API_URL  – base URL of the API  (default: http://localhost:3000)
//!   SYNAPSE_API_KEY  – tenant API key        (default: dev-key)
//!
//! Accepts an optional page size as the first CLI argument (default: 25).
//! Pass `--stream` as the second argument to iterate items via the
//! auto-following [`synapse_sdk::auto_follow`] stream instead of manually
//! looping over pages.
//!
//! Run with:
//!   cargo run --example transactions_list -- 50
//!   cargo run --example transactions_list -- 50 --stream
//!
//! Demonstrates cursor-based pagination and how an invalid/expired cursor must
//! be surfaced to the caller rather than retried with the same cursor.

use futures_util::StreamExt;
use synapse_sdk::{ListParams, SynapseClient, SynapseError};

#[tokio::main]
async fn main() {
    let base_url =
        std::env::var("SYNAPSE_API_URL").unwrap_or_else(|_| "http://localhost:3000".to_string());
    let api_key = std::env::var("SYNAPSE_API_KEY").unwrap_or_else(|_| "dev-key".to_string());
    let args: Vec<String> = std::env::args().collect();
    let limit: i64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(25);

    let client = SynapseClient::new(base_url, api_key);

    if args.get(2).map(String::as_str) == Some("--stream") {
        run_streamed(&client, limit).await;
        return;
    }

    let mut cursor: Option<String> = None;
    let mut page = 1;

    loop {
        let params = ListParams {
            cursor: cursor.clone(),
            limit: Some(limit),
            ..Default::default()
        };

        match client.transactions().list(params).await {
            Ok(result) => {
                println!("--- page {} ({} records) ---", page, result.data.len());
                for tx in &result.data {
                    println!(
                        "{}  {:<10}  {} {}",
                        tx.id, tx.status, tx.amount, tx.asset_code
                    );
                }

                match result.meta.next_cursor {
                    Some(next) if result.meta.has_more => {
                        cursor = Some(next);
                        page += 1;
                    }
                    // No more pages.
                    _ => break,
                }
            }
            // An invalid or expired cursor is non-retryable: surface it and stop.
            Err(SynapseError::InvalidCursor(msg)) => {
                eprintln!("cursor rejected — restart pagination from the beginning: {msg}");
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
    }
}

/// Iterate every transaction across all pages via the auto-following stream,
/// without manually tracking cursors.
async fn run_streamed(client: &SynapseClient, limit: i64) {
    let mut stream = Box::pin(client.transactions().list_all(ListParams {
        limit: Some(limit),
        ..Default::default()
    }));

    let mut count = 0;
    while let Some(tx) = stream.next().await {
        match tx {
            Ok(tx) => {
                count += 1;
                println!(
                    "{}  {:<10}  {} {}",
                    tx.id, tx.status, tx.amount, tx.asset_code
                );
            }
            Err(SynapseError::InvalidCursor(msg)) => {
                eprintln!("cursor rejected — restart pagination from the beginning: {msg}");
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
    }
    println!("--- {count} transactions total ---");
}
