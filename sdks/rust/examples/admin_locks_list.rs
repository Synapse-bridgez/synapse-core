//! Example: list active distributed locks via the admin API.
//!
//! Run with:
//! ```bash
//! SYNAPSE_API_KEY=pub-key SYNAPSE_ADMIN_KEY=admin-secret \
//!   cargo run --example admin_locks_list
//! ```

use synapse_sdk::client::SynapseClient;

#[tokio::main]
async fn main() {
    let base_url = std::env::var("SYNAPSE_BASE_URL")
        .unwrap_or_else(|_| "https://api.example.com".to_string());
    let api_key = std::env::var("SYNAPSE_API_KEY").expect("SYNAPSE_API_KEY required");
    let admin_key = std::env::var("SYNAPSE_ADMIN_KEY").expect("SYNAPSE_ADMIN_KEY required");

    let client = SynapseClient::builder(base_url, api_key)
        .admin_key(admin_key)
        .build();

    match client.admin().locks().list().await {
        Ok(response) => {
            println!(
                "Active locks: {} total, {} overdue",
                response.total, response.overdue
            );

            // The list is always a Vec — never null — so this is safe.
            if response.active_locks.is_empty() {
                println!("No locks currently held.");
            } else {
                for lock in &response.active_locks {
                    println!(
                        "  resource={} token={} acquired_at={} overdue={}",
                        lock.resource, lock.token, lock.acquired_at, lock.overdue
                    );
                }
            }
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}
