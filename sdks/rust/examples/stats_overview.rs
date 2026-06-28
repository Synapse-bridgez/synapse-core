//! Demonstrate `stats.status()`, `stats.daily()`, `stats.assets()`, `stats.cache_metrics()`.
//!
//! An empty dataset returns a valid zeroed structure — never null/None.
//!
//! Run with:
//!   cargo run --example stats_overview

use synapse_sdk::SynapseClient;
use synapse_sdk::models::DailyParams;

#[tokio::main]
async fn main() {
    let base_url =
        std::env::var("SYNAPSE_API_URL").unwrap_or_else(|_| "http://localhost:3000".to_string());
    let api_key = std::env::var("SYNAPSE_API_KEY").unwrap_or_else(|_| "dev-key".to_string());
    let client = SynapseClient::new(base_url, api_key);

    // status() — empty dataset returns [], not an error.
    match client.stats().status().await {
        Ok(counts) => {
            println!("=== Status counts ({} entries) ===", counts.len());
            for c in &counts {
                println!("  {}: {}", c.status, c.count);
            }
        }
        Err(e) => eprintln!("stats.status error: {e}"),
    }

    // daily() — defaults to last 7 days.
    match client.stats().daily(DailyParams { days: Some(7) }).await {
        Ok(totals) => {
            println!("=== Daily totals ({} days) ===", totals.len());
            for t in &totals {
                println!("  {}: {} txns, {} total", t.date, t.count, t.total_amount);
            }
        }
        Err(e) => eprintln!("stats.daily error: {e}"),
    }

    // assets()
    match client.stats().assets().await {
        Ok(stats) => {
            println!("=== Asset stats ({} assets) ===", stats.len());
            for s in &stats {
                println!("  {}: {} txns", s.asset_code, s.count);
            }
        }
        Err(e) => eprintln!("stats.assets error: {e}"),
    }

    // cache_metrics() — always returns a zeroed struct, never null.
    match client.stats().cache_metrics().await {
        Ok(m) => println!(
            "=== Cache metrics === hits={} misses={} hit_rate={:.1}%",
            m.hits,
            m.misses,
            m.hit_rate * 100.0
        ),
        Err(e) => eprintln!("stats.cache_metrics error: {e}"),
    }
}
