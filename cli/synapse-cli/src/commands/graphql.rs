use crate::client::SynapseCliClient;
use crate::formatter::{Formatter, OutputFormat};
use anyhow::Result;
use clap::{Args, Subcommand};

/// Top-level argument group for the `graphql` subcommand.
#[derive(Args)]
pub struct GraphqlCmd {
    #[command(subcommand)]
    pub command: GraphqlSubcommand,
}

/// Subcommands available under `synapse graphql`.
#[derive(Subcommand)]
pub enum GraphqlSubcommand {
    /// Send a raw GraphQL query to `POST /graphql` and print the response.
    ///
    /// Exit codes:
    ///   0 – success
    ///   1 – GraphQL application error (HTTP 200 with `errors` array) or network/HTTP error
    ///
    /// Output formats:
    ///   table – human-readable key/value output (default)
    ///   json  – pretty-printed JSON response body
    #[command(
        about = "Send a raw GraphQL query and print the response",
        long_about = "Send a raw GraphQL query to POST /graphql and print the result.\n\n\
                      Exit codes:\n  \
                      0 - Success\n  \
                      1 - GraphQL application error or network/HTTP failure\n\n\
                      Output formats:\n  \
                      table - Human-readable output (default)\n  \
                      json  - Pretty-printed JSON"
    )]
    Query {
        /// The GraphQL query string (e.g. \"{ transactions { id status } }\")
        #[arg(long)]
        query: String,

        /// Output format: 'table' (default) or 'json'
        #[arg(long, default_value = "table")]
        format: String,
    },
}

// ── Runner ─────────────────────────────────────────────────────────────────────

pub async fn run(cmd: GraphqlSubcommand, base_url: &str) -> Result<()> {
    let client = SynapseCliClient::new(base_url);

    match cmd {
        GraphqlSubcommand::Query { query, format } => {
            let body = serde_json::json!({ "query": query, "variables": null });
            let response: serde_json::Value = client.post_json("/graphql", &body).await?;

            let fmt = OutputFormat::from_str(&format);

            // Surface application-level GraphQL errors (HTTP 200 + errors array).
            if let Some(errors) = response.get("errors").and_then(|e| e.as_array()) {
                if !errors.is_empty() {
                    let msg = errors
                        .iter()
                        .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
                        .collect::<Vec<_>>()
                        .join("; ");
                    anyhow::bail!("graphql error: {}", msg);
                }
            }

            let output = Formatter::format_json_output(&response, fmt)?;
            println!("{}", output);

            Ok(())
        }
    }
}
