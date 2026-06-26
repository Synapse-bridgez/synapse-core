//! Fetch a single transaction by ID and print its fields.
//!
//! Reads configuration from environment variables:
//!   SYNAPSE_API_URL  – base URL of the API  (default: http://localhost:3000)
//!   SYNAPSE_API_KEY  – tenant API key        (default: dev-key)
//!
//! Accepts the transaction UUID as the first CLI argument.
//!
//! Run with:
//!   cargo run --example transactions_get -- <transaction-id>
//!
//! Handling the 404 case is demonstrated explicitly so callers can see how to
//! distinguish a missing record from other failures.

use synapse_sdk::{SynapseClient, SynapseError};

#[tokio::main]
async fn main() {
    let base_url = std::env::var("SYNAPSE_API_URL")
        .unwrap_or_else(|_| "http://localhost:3000".to_string());
    let api_key =
        std::env::var("SYNAPSE_API_KEY").unwrap_or_else(|_| "dev-key".to_string());
    let tx_id = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "550e8400-e29b-41d4-a716-446655440000".to_string());

    let client = SynapseClient::new(base_url, api_key);

    match client.transactions().get(&tx_id).await {
        Ok(tx) => {
            println!("id:          {}", tx.id);
            println!("status:      {}", tx.status);
            println!("amount:      {} {}", tx.amount, tx.asset_code);
            println!("account:     {}", tx.stellar_account);
            println!("created_at:  {}", tx.created_at);
        }
        Err(SynapseError::NotFound(msg)) => {
            eprintln!("transaction not found: {}", msg);
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    }
}
