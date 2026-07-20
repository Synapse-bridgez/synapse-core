//! Search transactions by filter, with support for pagination and empty results.
//!
//! Reads configuration from environment variables:
//!   SYNAPSE_API_URL  – base URL of the API  (default: http://localhost:3000)
//!   SYNAPSE_API_KEY  – tenant API key        (default: dev-key)
//!
//! Run with:
//!   cargo run --example transactions_search
//!
//! Demonstrates:
//!   - Filtering by status, asset code, and amount range
//!   - Paginating through multi-page result sets via `next_cursor`
//!   - Handling a query that matches zero records (empty page, not an error)

use synapse_sdk::{SearchParams, SynapseClient, SynapseError};

#[tokio::main]
async fn main() {
    let base_url =
        std::env::var("SYNAPSE_API_URL").unwrap_or_else(|_| "http://localhost:3000".to_string());
    let api_key = std::env::var("SYNAPSE_API_KEY").unwrap_or_else(|_| "dev-key".to_string());

    let client = SynapseClient::new(base_url, api_key);

    // ── First search: look for completed USD transactions of at least $10 ──
    println!("--- Search 1: completed USD transactions ≥ $10 ---");

    let filters = SearchParams {
        status: Some("completed".to_string()),
        asset_code: Some("USD".to_string()),
        min_amount: Some("10.00".to_string()),
        ..Default::default()
    };

    match client.transactions().search(filters).await {
        Ok(page) => {
            println!("total matches across all pages: {}", page.total);

            if page.results.is_empty() {
                println!("no results on this page");
            } else {
                for tx in &page.results {
                    println!(
                        "  {}  {}  {} {}",
                        tx.id, tx.status, tx.amount, tx.asset_code
                    );
                }
            }

            // ── Pagination: follow next_cursor until exhausted ──
            let mut cursor = page.next_cursor;
            while let Some(token) = cursor {
                let page = client
                    .transactions()
                    .search(SearchParams {
                        cursor: Some(token),
                        ..Default::default()
                    })
                    .await
                    .unwrap_or_else(|e| {
                        eprintln!("pagination error: {e}");
                        std::process::exit(1);
                    });

                for tx in &page.results {
                    println!(
                        "  {}  {}  {} {}",
                        tx.id, tx.status, tx.amount, tx.asset_code
                    );
                }
                cursor = page.next_cursor;
            }
        }
        Err(e) => {
            eprintln!("search error: {e}");
            std::process::exit(1);
        }
    }

    // ── Second search: query that matches nothing (not an error) ──
    println!("\n--- Search 2: nonexistent status (expect zero matches) ---");

    let filters = SearchParams {
        status: Some("nonexistent_status".to_string()),
        ..Default::default()
    };

    match client.transactions().search(filters).await {
        Ok(page) => {
            // Zero matches is a successful response with total=0 and empty results.
            println!(
                "total: {}  results: {}  has_next: {}",
                page.total,
                page.results.len(),
                page.next_cursor.is_some(),
            );

            if page.total == 0 {
                println!("(expected: no records matched the filter)");
            }
        }
        Err(SynapseError::InvalidCursor(msg)) => {
            eprintln!("cursor rejected: {msg}");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}
