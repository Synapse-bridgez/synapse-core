//! List settlements page-by-page and print each row.
//!
//! Reads configuration from environment variables:
//!   SYNAPSE_API_URL  – base URL of the API  (default: http://localhost:3000)
//!   SYNAPSE_API_KEY  – tenant API key        (default: dev-key)
//!
//! Accepts an optional page size as the first CLI argument (default: 10).
//!
//! Run with:
//!   cargo run --example settlements_list -- 25
//!
//! Demonstrates cursor-based pagination and how an invalid/expired cursor must
//! be surfaced to the caller rather than retried with the same cursor.

use synapse_sdk::{SettlementParams, SynapseClient, SynapseError};

#[tokio::main]
async fn main() {
    let base_url =
        std::env::var("SYNAPSE_API_URL").unwrap_or_else(|_| "http://localhost:3000".to_string());
    let api_key = std::env::var("SYNAPSE_API_KEY").unwrap_or_else(|_| "dev-key".to_string());
    let limit: i64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);

    let client = SynapseClient::new(base_url, api_key);

    let mut cursor: Option<String> = None;
    let mut page = 1;

    loop {
        let params = SettlementParams {
            cursor: cursor.clone(),
            limit: Some(limit),
            ..Default::default()
        };

        match client.settlements().list(params).await {
            Ok(result) => {
                println!("--- page {} ({} records) ---", page, result.settlements.len());
                for s in &result.settlements {
                    println!(
                        "{}  {:<12}  {} {}",
                        s.id, s.status, s.total_amount, s.asset_code
                    );
                }

                match result.next_cursor {
                    Some(next) if result.has_more => {
                        cursor = Some(next);
                        page += 1;
                    }
                    _ => break,
                }
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
}
