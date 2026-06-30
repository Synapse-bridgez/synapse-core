//! Demonstrates `settlements.list()` and `settlements.get(id)`.
//!
//! Run with:
//!   cargo run --example settlements_list_get
//!
//! A 404 on an unknown settlement ID is surfaced as [`SynapseError::NotFound`],
//! which is distinct from any transport-level [`SynapseError::Network`] error.

use synapse_sdk::{SynapseClient, SynapseError};

#[tokio::main]
async fn main() {
    let client = SynapseClient::new(
        std::env::var("SYNAPSE_BASE_URL").unwrap_or_else(|_| "https://api.example.com".into()),
        std::env::var("SYNAPSE_API_KEY").unwrap_or_else(|_| "your-api-key".into()),
    );

    // ── List settlements (first page) ────────────────────────────────────────
    match client.settlements().list(None, Some(10)).await {
        Ok(page) => {
            println!("settlements on page: {}", page.settlements.len());
            for s in &page.settlements {
                println!("  {} {} {}", s.id, s.status, s.total_amount);
            }
            if page.meta.has_more {
                println!("more pages available; next_cursor: {:?}", page.meta.next_cursor);
            }
        }
        Err(e) => eprintln!("list error: {e}"),
    }

    // ── Fetch a single settlement by ID ──────────────────────────────────────
    let id = "550e8400-e29b-41d4-a716-446655440000";
    match client.settlements().get(id).await {
        Ok(s) => println!("settlement {id} status: {}", s.status),
        // A missing ID is clearly distinguishable from a transport error.
        Err(SynapseError::NotFound(msg)) => eprintln!("not found: {msg}"),
        Err(SynapseError::Network(e)) => eprintln!("transport error: {e}"),
        Err(e) => eprintln!("error: {e}"),
    }
}
