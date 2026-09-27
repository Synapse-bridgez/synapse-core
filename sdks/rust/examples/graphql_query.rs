//! Execute a GraphQL query and print the response data.
//!
//! Reads configuration from environment variables:
//!   SYNAPSE_API_URL  – base URL of the API  (default: http://localhost:3000)
//!   SYNAPSE_API_KEY  – tenant API key        (default: dev-key)
//!
//! Run with:
//!   cargo run --example graphql_query
//!
//! GraphQL errors (HTTP 200 + `"errors"` array) are surfaced as
//! `SynapseError::GraphQL` and are distinct from transport failures.

use synapse_sdk::{
    SynapseClient, SynapseError, TransactionField, TransactionQueryBuilder, TransactionQueryFilter,
};

#[tokio::main]
async fn main() {
    let base_url =
        std::env::var("SYNAPSE_API_URL").unwrap_or_else(|_| "http://localhost:3000".to_string());
    let api_key = std::env::var("SYNAPSE_API_KEY").unwrap_or_else(|_| "dev-key".to_string());

    let client = SynapseClient::new(base_url, api_key);

    // Hand-written query string.
    let query = r#"{ transactions { id status } }"#;

    match client.graphql().query(query, None).await {
        Ok(resp) => {
            println!(
                "data: {}",
                serde_json::to_string_pretty(&resp.data).unwrap()
            );
        }
        // GraphQL-level errors come back as HTTP 200 with an `errors` array.
        // They must be handled separately from transport/network failures.
        Err(SynapseError::GraphQL(msg)) => {
            eprintln!("GraphQL errors: {}", msg);
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("transport error: {}", e);
            std::process::exit(1);
        }
    }

    // Equivalent query via the typed builder: field names and the overall
    // query shape are built from `TransactionField`/`TransactionQueryFilter`
    // instead of being hand-typed into a raw string. The builder only
    // produces the query string — send it the same way as any other query.
    let filter = TransactionQueryFilter {
        status: Some("completed".to_string()),
        ..Default::default()
    };
    let builder =
        TransactionQueryBuilder::new().select(&[TransactionField::Id, TransactionField::Status]);
    let built_query = builder.list_query(Some(&filter), Some(20), None);

    match client.graphql().query(built_query, None).await {
        Ok(resp) => println!(
            "builder result: {}",
            serde_json::to_string_pretty(&resp.data).unwrap()
        ),
        Err(SynapseError::GraphQL(msg)) => eprintln!("GraphQL errors: {}", msg),
        Err(e) => eprintln!("transport error: {}", e),
    }
}
