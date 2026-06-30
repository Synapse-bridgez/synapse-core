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
