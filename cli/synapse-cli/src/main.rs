use synapse_cli::commands::{Cli, Commands};
use clap::Parser;
use clap::{Parser, Subcommand};
use synapse_cli::{CliConfig, OutputFormat};

mod handlers {
    use super::{GraphqlCmd, SettlementsCmd, TransactionsCmd};
    use synapse_cli::{CliConfig, Formatter, OutputFormat, SynapseCliClient};

    pub async fn handle_transactions(
        command: TransactionsCmd,
        config: &CliConfig,
        _output_format: OutputFormat,
    ) -> anyhow::Result<()> {
        match command {
            TransactionsCmd::Export {
                format,
                from,
                to,
                status,
                asset_code,
                output,
            } => {
                let client = SynapseCliClient::new(&config.base_url);

                let mut query_params: Vec<(&str, String)> = Vec::new();
                query_params.push(("format", format.clone()));

                let from_owned;
                if let Some(ref f) = from {
                    from_owned = f.clone();
                    query_params.push(("from", from_owned.clone()));
                }

                let to_owned;
                if let Some(ref t) = to {
                    to_owned = t.clone();
                    query_params.push(("to", to_owned.clone()));
                }

                let status_owned;
                if let Some(ref s) = status {
                    status_owned = s.clone();
                    query_params.push(("status", status_owned.clone()));
                }

                let asset_code_owned;
                if let Some(ref ac) = asset_code {
                    asset_code_owned = ac.clone();
                    query_params.push(("asset_code", asset_code_owned.clone()));
                }

                let query_refs: Vec<(&str, &str)> = query_params
                    .iter()
                    .map(|(k, v)| (*k, v.as_str()))
                    .collect();

                let bytes = client.get_bytes("/export", &query_refs).await?;

                if let Some(output_path) = output {
                    std::fs::write(&output_path, &bytes)?;
                    println!("✓ Exported to {}", output_path);
                } else {
                    let output = String::from_utf8(bytes)?;
                    println!("{}", output);
                }

                Ok(())
            }
        }
    }

    pub async fn handle_settlements(
        command: SettlementsCmd,
        config: &CliConfig,
        _output_format: OutputFormat,
    ) -> anyhow::Result<()> {
        let client = SynapseCliClient::new(&config.base_url);

        match command {
            SettlementsCmd::List {
                cursor,
                limit,
                direction,
                format,
            } => {
                let mut query_params: Vec<(&str, String)> = Vec::new();
                query_params.push(("limit", limit.to_string()));
                query_params.push(("direction", direction.clone()));

                let cursor_owned;
                if let Some(ref c) = cursor {
                    cursor_owned = c.clone();
                    query_params.push(("cursor", cursor_owned.clone()));
                }

                let query_refs: Vec<(&str, &str)> = query_params
                    .iter()
                    .map(|(k, v)| (*k, v.as_str()))
                    .collect();

                let fmt = OutputFormat::from_str(&format);
                let response: serde_json::Value =
                    client.get_with_query("/settlements", &query_refs).await?;

                let output = Formatter::format_json_output(&response, fmt)?;
                println!("{}", output);

                Ok(())
            }

            SettlementsCmd::Get {
                settlement_id,
                format,
            } => {
                let fmt = OutputFormat::from_str(&format);
                let path = format!("/settlements/{}", settlement_id);
                let response: serde_json::Value = client.get_json(&path).await?;

                let output = Formatter::format_json_output(&response, fmt)?;
                println!("{}", output);

                Ok(())
            }
        }
    }

    pub async fn handle_graphql(
        command: GraphqlCmd,
        config: &CliConfig,
    ) -> anyhow::Result<()> {
        let client = SynapseCliClient::new(&config.base_url);

        match command {
            GraphqlCmd::Query { query, format } => {
                let body = serde_json::json!({ "query": query, "variables": null });
                let response: serde_json::Value = client.post_json("/graphql", &body).await?;

                let fmt = OutputFormat::from_str(&format);

                // Surface application-level GraphQL errors (HTTP 200 + errors array)
                if let Some(errors) = response.get("errors") {
                    if let Some(arr) = errors.as_array() {
                        if !arr.is_empty() {
                            let msg = arr
                                .iter()
                                .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
                                .collect::<Vec<_>>()
                                .join("; ");
                            anyhow::bail!("graphql error: {}", msg);
                        }
                    }
                }

                let output = Formatter::format_json_output(&response, fmt)?;
                println!("{}", output);

                Ok(())
            }
        }
    }
}

#[derive(Parser)]
#[command(name = "synapse")]
#[command(about = "Synapse CLI - Transaction, Settlement, and GraphQL management")]
#[command(version)]
struct Cli {
    /// Base URL for the Synapse API
    #[arg(long, env = "SYNAPSE_URL")]
    url: Option<String>,

    /// API key for authentication
    #[arg(long, env = "SYNAPSE_API_KEY")]
    api_key: Option<String>,

    /// Output format (table or json)
    #[arg(long, default_value = "table", global = true)]
    format: String,
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

mod formatter;
mod output;

use formatter::{Formatter, OutputFormat};

#[derive(Parser, Debug)]
#[command(name = "synapse", version, about = "Synapse CLI")]
struct Cli {
    /// Base URL of the Synapse API.
    #[arg(
        long = "base-url",
        alias = "url",
        env = "SYNAPSE_BASE_URL",
        default_value = "http://127.0.0.1:3000"
    )]
    base_url: String,

    /// API key for authenticated requests.
    #[arg(long, env = "SYNAPSE_API_KEY", default_value = "")]
    api_key: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Admin operations.
    #[command(subcommand)]
    Admin(AdminCommands),

    /// Transaction commands.
    #[command(subcommand)]
    Transactions(TransactionsCommands),

    /// Settlement commands.
    #[command(subcommand)]
    Settlements(SettlementsCommands),

    /// Generate shell completion scripts.
    Completions {
        /// Shell to generate completions for: bash, zsh, or fish.
        shell: String,
    },
}

#[derive(Subcommand, Debug)]
enum AdminCommands {
    /// Reconciliation reports and runs.
    #[command(subcommand)]
    Reconciliation(ReconciliationCommands),

    /// Settlement administration.
    #[command(subcommand)]
    Settlements(AdminSettlementCommands),

    /// Distributed lock administration.
    #[command(subcommand)]
    Locks(LockCommands),

    /// Tenant quota administration.
    #[command(subcommand)]
    Quotas(QuotaCommands),
}

#[derive(Subcommand, Debug)]
enum AdminSettlementCommands {
    /// Update a settlement status through the admin API.
    UpdateStatus {
        /// Settlement UUID.
        settlement_id: Uuid,

        /// New settlement status.
        new_status: String,

        /// Optional reason recorded with the status change.
        #[arg(long)]
        reason: Option<String>,

        /// Optional adjusted total amount. Only meaningful for adjusted settlements.
        #[arg(long)]
        new_total: Option<String>,

        /// Actor recorded with the status change. Defaults to the server's admin actor.
        #[arg(long)]
        actor: Option<String>,

        /// Print the raw API response as JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum Commands {
    /// Transaction management commands
    Transactions {
        #[command(subcommand)]
        command: TransactionsCmd,
    },

    /// Settlement management commands
    Settlements {
        #[command(subcommand)]
        command: SettlementsCmd,
    },

    /// Send a raw GraphQL query to the Synapse API
    Graphql {
        #[command(subcommand)]
        command: GraphqlCmd,
    },
}

#[derive(Subcommand)]
enum TransactionsCmd {
    /// Export transactions to CSV or JSON format with optional filters.
    Export {
        /// Export format: 'csv' (default) or 'json'
        #[arg(long, default_value = "csv")]
        format: String,

        /// Start date filter (inclusive). Format: YYYY-MM-DD.
        #[arg(long)]
        from: Option<String>,

        /// End date filter (inclusive). Format: YYYY-MM-DD.
        #[arg(long)]
        to: Option<String>,

        /// Filter by transaction status (e.g., pending, completed).
        #[arg(long)]
        status: Option<String>,

        /// Filter by asset code (e.g., USD, EUR, USDC).
        #[arg(long)]
        asset_code: Option<String>,

        /// Output file path. Default: stdout.
#[derive(Subcommand, Debug)]
enum LockCommands {
    #[command(
        about = "List active distributed locks",
        long_about = "List active distributed locks currently held by this Synapse instance.\n\nRequired flags: none.\nOptional flags:\n  --json            Print the raw API response as pretty JSON instead of the default table.\n\nOutput fields:\n  resource          Protected resource name for the lock.\n  token             Lock owner token.\n  acquired_at       Unix timestamp, in seconds, when the lock was acquired.\n  ttl_secs          Lock TTL in seconds.\n  expected_duration_secs  Expected lock hold duration in seconds.\n  overdue           Whether the lock has exceeded twice its expected duration."
    )]
    Reconciliation(ReconciliationCommands),

    /// Synapse event stream commands.
    #[command(
        about = "Event stream commands",
        long_about = "Watch or query the Synapse event stream."
    )]
    Events(EventsCommands),
}

#[derive(Subcommand, Debug)]
enum EventsCommands {
    /// Fetch recent transaction status-change events.
    ///
    /// Calls GET /events and prints each event's transaction_id, status, and
    /// timestamp.  An empty event list is a valid response, not an error.
    ///
    /// Exit codes:
    ///   0 - Success (including empty list)
    ///   1 - Server error or network failure
    ///
    /// Example:
    ///   synapse admin events watch
    ///   synapse admin events watch --json
    #[command(
        about = "Fetch recent transaction status-change events",
        long_about = "Fetch recent transaction status-change events from GET /events.\n\n\
                      Exit codes:\n  0 - Success\n  1 - Server error\n\n\
                      Edge case: an empty event list is valid and exits with 0."
    )]
    Watch {
        /// Print the raw API response as JSON instead of a table.
    List {
        /// Print the raw API response as JSON.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
enum QuotaCommands {
    /// List quota usage for all active tenants.
    List {
        /// Print the raw API response as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Attempt to reconnect a WebSocket session (`POST /reconnect`).
    ///
    /// Sends the opaque `cursor` (session ID) from a previous connection to the
    /// server. The server validates the session and returns backoff guidance and
    /// whether a full state resync is required.
    ///
    /// Edge cases:
    ///   - An expired session returns status `session_expired`, not an error.
    ///   - An invalid cursor returns status `invalid_token`, not an error.
    ///
    /// Exit codes:
    ///   0 - success (including expired/invalid token responses)
    ///   1 - network or server error
    ///
    /// Example:
    ///   synapse admin events reconnect --cursor 550e8400-e29b-41d4-a716-446655440000
    ///   synapse admin events reconnect --cursor 550e8400-e29b-41d4-a716-446655440000 --json
    #[command(
        name = "reconnect",
        about = "Attempt to reconnect a WebSocket session (POST /reconnect)",
        long_about = "Sends the cursor from a previous connection to POST /reconnect.\n\n\
                      Exit codes:\n  0 - Success (including session_expired / invalid_token)\n  \
                      1 - Network or server error"
    )]
    Reconnect {
        /// Opaque session cursor (UUID) obtained from a previous reconnect-status call.
        #[arg(long, value_name = "CURSOR")]
        cursor: String,

        /// Print output as JSON instead of a table.
    /// Get quota usage for one tenant.
    Get {
        /// Tenant UUID.
        tenant_id: Uuid,

        /// Print the raw API response as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Set a tenant quota override.
    Set {
        /// Tenant UUID.
        tenant_id: Uuid,

        /// Positive quota limit.
        #[arg(value_name = "LIMIT", conflicts_with = "limit_flag")]
        limit: Option<u32>,

        /// Positive quota limit.
        #[arg(long = "limit", value_name = "LIMIT")]
        limit_flag: Option<u32>,

        /// Print the raw API response as JSON.
        #[arg(long)]
        json: bool,
    },

    /// Check reconnection status without committing an attempt (`GET /reconnect/status`).
    ///
    /// When there is no active session (no cursor supplied), the server creates a
    /// fresh session and returns `status: ready`. Callers should inspect the
    /// `status` field to decide how to proceed.
    ///
    /// Edge case: calling without `--cursor` is always valid; the server returns a
    /// clean `ready` response.
    ///
    /// Exit codes:
    ///   0 - success (including no-session case)
    ///   1 - network or server error
    ///
    /// Example:
    ///   synapse admin events reconnect-status
    ///   synapse admin events reconnect-status --cursor 550e8400-e29b-41d4-a716-446655440000
    ///   synapse admin events reconnect-status --json
    #[command(
        name = "reconnect-status",
        about = "Check reconnection status without committing an attempt (GET /reconnect/status)",
        long_about = "Queries GET /reconnect/status.\n\n\
                      Omit --cursor to get a fresh ready status (no active session required).\n\n\
                      Exit codes:\n  0 - Success (including no-session case)\n  \
                      1 - Network or server error"
    )]
    ReconnectStatus {
        /// Optional opaque session cursor to query status for a specific session.
        /// Omit to get a fresh ready status (no active session required).
        #[arg(long, value_name = "CURSOR")]
        cursor: Option<String>,

        /// Print output as JSON instead of a table.
    /// Reset the current usage counter for one tenant.
    Reset {
        /// Tenant UUID.
        tenant_id: Uuid,

        /// Print the raw API response as JSON.
        #[arg(long)]
        json: bool,
    },
}

// ── Reconciliation subcommands ────────────────────────────────────────────────

#[derive(Subcommand, Debug)]
enum ReconciliationCommands {
    #[command(
        about = "List reconciliation reports",
        long_about = "List reconciliation reports generated by the admin API.\n\nRequired flags: none.\nOptional flags:\n  --limit <LIMIT>   Maximum number of reports to return (default: 20).\n  --offset <OFFSET>  Number of reports to skip before returning results (default: 0).\n  --json            Print the raw API response as JSON."
    )]
    Reports {
        #[arg(long, value_name = "LIMIT", default_value_t = 20)]
        limit: u32,

        #[arg(long, value_name = "OFFSET", default_value_t = 0)]
        offset: u32,

        #[arg(long)]
        json: bool,
    },

    #[command(
        about = "Show a reconciliation report",
        long_about = "Fetch one reconciliation report by UUID and print the full response body.\n\nRequired flags:\n  <REPORT_ID>       UUID of the report to fetch.\nOptional flags:\n  --json            Print the raw API response as JSON."
    )]
    Report {
        #[arg(value_name = "REPORT_ID")]
        report_id: Uuid,

        #[arg(long)]
        json: bool,
    },

    #[command(
        about = "Run a reconciliation report",
        long_about = "Run a reconciliation pass for one Stellar account and persist the result.\n\nRequired flags:\n  --account <ACCOUNT>      Stellar account to reconcile.\nOptional flags:\n  --period-hours <HOURS>   Hours of history to include (default: 24).\n  --json                   Print the raw API response as JSON."
    )]
    Run {
        #[arg(long, value_name = "ACCOUNT")]
        account: String,

        #[arg(long, value_name = "HOURS", default_value_t = 24)]
        period_hours: u32,

        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
enum TransactionsCommands {
    /// Export transactions to CSV or JSON format with optional filters.
    Export {
        /// Export format: csv or json.
        #[arg(long, default_value = "csv")]
        format: String,

        /// Start date filter, inclusive, in YYYY-MM-DD format.
        #[arg(long)]
        from: Option<String>,

        /// End date filter, inclusive, in YYYY-MM-DD format.
        #[arg(long)]
        to: Option<String>,

        /// Filter by transaction status.
        #[arg(long)]
        status: Option<String>,

        /// Filter by asset code.
        #[arg(long)]
        asset_code: Option<String>,

        /// Save output to a file instead of stdout.
        #[arg(long)]
        output: Option<String>,
    },
}

#[derive(Subcommand)]
enum SettlementsCmd {
    /// List settlements with cursor-based pagination.
    List {
        /// Pagination cursor from a previous response.
        #[arg(long)]
        cursor: Option<String>,

        /// Number of results per page (1-100, default 10).
        #[arg(long, default_value = "10")]
        limit: i64,

        /// Pagination direction: 'forward' (default) or 'backward'.
        #[arg(long, default_value = "forward")]
        direction: String,

        /// Output format: 'table' (default) or 'json'.
#[derive(Subcommand, Debug)]
enum SettlementsCommands {
    /// List settlements with cursor-based pagination.
    List {
        #[arg(long)]
        cursor: Option<String>,

        #[arg(long, default_value_t = 10)]
        limit: i64,

        #[arg(long, default_value = "forward")]
        direction: String,

        #[arg(long, default_value = "table")]
        format: String,
    },

    /// Get a specific settlement by ID.
    Get {
        /// Settlement UUID.
        settlement_id: String,

        /// Output format: 'table' (default) or 'json'.
        #[arg(long, default_value = "table")]
        format: String,
    },
}

/// GraphQL subcommand — send raw queries to `POST /graphql`.
///
/// Exit codes:
///   0 – success
///   1 – GraphQL application error (HTTP 200 with `errors` array) or network/HTTP error
///
/// Output formats:
///   table – human-readable key/value output (default)
///   json  – pretty-printed JSON response
#[derive(Subcommand)]
enum GraphqlCmd {
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
        settlement_id: String,

        #[arg(long, default_value = "table")]
        format: String,
    },
}

#[derive(Debug, Deserialize, Serialize)]
struct TenantQuotaView {
    tenant_id: Uuid,
    name: String,
    rate_limit_per_minute: i32,
    quota_status: Option<serde_json::Value>,
}

    let cli = Cli::parse();
    let base_url = &cli.base_url;
    let api_key = &cli.api_key;

    match cli.command {
        Commands::Health(cmd) => {
            synapse_cli::commands::health::run(cmd, base_url, api_key).await?;
        }
        Commands::Stats(cmd) => {
            synapse_cli::commands::stats::run(cmd, base_url, api_key).await?;
        }
        Commands::Settlements(cmd) => {
            synapse_cli::commands::settlements::run(cmd.command, base_url, api_key).await?;
        }
        Commands::Transactions(cmd) => {
            synapse_cli::commands::transactions::run(cmd.command, base_url, api_key).await?;
        }
    }

    Ok(())
#[derive(Debug, Deserialize, Serialize)]
struct ListLocksResponse {
    active_locks: Vec<ActiveLockView>,
    total: usize,
    overdue: usize,
}

#[derive(Debug, Deserialize, Serialize)]
struct ActiveLockView {
    resource: String,
    token: String,
    acquired_at: u64,
    ttl_secs: u64,
    expected_duration_secs: u64,
    overdue: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct ListReportsResponse {
    reports: Vec<ReportSummary>,
    total: i64,
    limit: i32,
    offset: i32,
}

#[derive(Debug, Deserialize, Serialize)]
struct ReportSummary {
    id: Uuid,
    generated_at: String,
    period_start: String,
    period_end: String,
    total_db_transactions: i32,
    total_chain_payments: i32,
    missing_on_chain_count: i32,
    orphaned_payments_count: i32,
    amount_mismatches_count: i32,
    has_discrepancies: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct ReportDetailResponse {
    id: Uuid,
    generated_at: String,
    period_start: String,
    period_end: String,
    summary: ReportDetailSummary,
    missing_on_chain: Vec<serde_json::Value>,
    orphaned_payments: Vec<serde_json::Value>,
    amount_mismatches: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ReportDetailSummary {
    total_db_transactions: usize,
    total_chain_payments: usize,
    missing_on_chain_count: i32,
    orphaned_payments_count: i32,
    amount_mismatches_count: i32,
    has_discrepancies: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct RunResponse {
    message: String,
    report: ReportSummary,
}

#[derive(Debug, Serialize)]
struct RunRequest<'a> {
    account: &'a str,
    period_hours: Option<u32>,
}

#[derive(Debug, Serialize)]
struct UpdateSettlementStatusRequest<'a> {
    status: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_total: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    actor: Option<&'a str>,
}

struct ApiClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl ApiClient {
    fn new(base_url: String, api_key: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key,
        }
    }

    async fn get<T>(&self, path: &str) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        self.send(self.http.get(self.url(path))).await
    }

    async fn put_json<T>(&self, path: &str, body: serde_json::Value) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        self.send(self.http.put(self.url(path)).json(&body)).await
    }

    async fn post_json<T, B>(&self, path: &str, body: &B) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
        B: Serialize + ?Sized,
    {
        self.send(self.http.post(self.url(path)).json(body)).await
    }

    async fn patch_json<T, B>(&self, path: &str, body: &B) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
        B: Serialize + ?Sized,
    {
        self.send(self.http.patch(self.url(path)).json(body)).await
    }

    async fn delete<T>(&self, path: &str) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        self.send(self.http.delete(self.url(path))).await
    }

    async fn get_bytes(&self, path: &str, query: &[(&str, String)]) -> Result<Vec<u8>> {
        let response = self
            .with_auth(self.http.get(self.url(path)).query(query))
            .send()
            .await
            .context("request failed")?;

        let status = response.status();
        let body = response
            .bytes()
            .await
            .context("failed to read response body")?;
        if !status.is_success() {
            bail!(
                "server returned {status}: {}",
                String::from_utf8_lossy(&body)
            );
        }

        Ok(body.to_vec())
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn with_auth(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.api_key.is_empty() {
            request
        } else {
            request.header("X-API-Key", &self.api_key)
        }
    }

    async fn send<T>(&self, request: reqwest::RequestBuilder) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let response = self
            .with_auth(request)
            .send()
            .await
            .context("request failed")?;
        let status = response.status();
        let body = response
            .text()
            .await
            .context("failed to read response body")?;

        if !status.is_success() {
            bail!("{}", server_error_message(&body));
        }

        serde_json::from_str(&body).context("failed to parse response JSON")
    }
}

fn server_error_message(body: &str) -> String {
    let message = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            ["error", "detail", "message"]
                .into_iter()
                .find_map(|key| value.get(key).and_then(serde_json::Value::as_str))
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.to_string());

    message
        .strip_prefix("Bad request: ")
        .unwrap_or(&message)
        .to_string()
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = ApiClient::new(cli.base_url, cli.api_key);

    match cli.command {
        Commands::Admin(admin) => match admin {
            AdminCommands::Reconciliation(command) => {
                handle_reconciliation(&client, &base_url, command).await?
            }
            AdminCommands::Events(command) => {
                handle_events(&client, &base_url, command).await?
            }
        },
        Commands::Admin(command) => handle_admin(&client, command).await?,
        Commands::Transactions(command) => handle_transactions(&client, command).await?,
        Commands::Settlements(command) => handle_settlements(&client, command).await?,
        Commands::Completions { shell } => print_completions(&shell)?,
    }

    Ok(())
}

async fn handle_admin(client: &ApiClient, command: AdminCommands) -> Result<()> {
    match command {
        AdminCommands::Reconciliation(command) => handle_reconciliation(client, command).await,
        AdminCommands::Settlements(command) => handle_admin_settlements(client, command).await,
        AdminCommands::Locks(command) => handle_locks(client, command).await,
        AdminCommands::Quotas(command) => handle_quotas(client, command).await,
    }
}

async fn handle_admin_settlements(
    client: &ApiClient,
    command: AdminSettlementCommands,
) -> Result<()> {
    match command {
        AdminSettlementCommands::UpdateStatus {
            settlement_id,
            new_status,
            reason,
            new_total,
            actor,
            json,
        } => {
            let response: serde_json::Value = client
                .patch_json(
                    &format!("/admin/settlements/{settlement_id}/status"),
                    &UpdateSettlementStatusRequest {
                        status: &new_status,
                        reason: reason.as_deref(),
                        new_total: new_total.as_deref(),
                        actor: actor.as_deref(),
                    },
                )
                .await?;
            println!(
                "{}",
                Formatter::format_json_output(&response, OutputFormat::from_json_flag(json))?
            );
        }
    }

    Ok(())
}

async fn handle_locks(client: &ApiClient, command: LockCommands) -> Result<()> {
    match command {
        LockCommands::List { json } => {
            let response: ListLocksResponse = client.get("/admin/locks").await?;
            println!("{}", output::render(&response, json, format_locks_table)?);
        }
    }

    Ok(())
}

async fn handle_quotas(client: &ApiClient, command: QuotaCommands) -> Result<()> {
    match command {
        QuotaCommands::List { json } => {
            let response: Vec<TenantQuotaView> = client.get("/admin/quotas").await?;
            println!(
                "{}",
                Formatter::format_json_output(&response, OutputFormat::from_json_flag(json))?
            );
        }
        QuotaCommands::Get { tenant_id, json } => {
            let response: TenantQuotaView =
                client.get(&format!("/admin/quotas/{tenant_id}")).await?;
            println!(
                "{}",
                Formatter::format_json_output(&response, OutputFormat::from_json_flag(json))?
            );
        }
        QuotaCommands::Set {
            tenant_id,
            limit,
            limit_flag,
            json: as_json,
        } => {
            let Some(limit) = limit.or(limit_flag) else {
                bail!("quota limit is required");
            };

            if limit == 0 {
                bail!("quota limit must be positive");
            }

            let response: serde_json::Value = client
                .put_json(
                    &format!("/admin/quotas/{tenant_id}"),
                    json!({ "custom_limit": limit }),
                )
                .await?;
            println!(
                "{}",
                Formatter::format_json_output(&response, OutputFormat::from_json_flag(as_json))?
            );
        }
        QuotaCommands::Reset { tenant_id, json } => {
            let response: serde_json::Value = client
                .delete(&format!("/admin/quotas/{tenant_id}/reset"))
                .await?;
            println!(
                "{}",
                Formatter::format_json_output(&response, OutputFormat::from_json_flag(json))?
            );
        }
    }

    Ok(())
}

async fn handle_reconciliation(client: &ApiClient, command: ReconciliationCommands) -> Result<()> {
    match command {
        ReconciliationCommands::Reports {
            limit,
            offset,
            json,
        } => {
            let response: ListReportsResponse = client
                .get(&format!(
                    "/admin/reconciliation/reports?limit={limit}&offset={offset}"
                ))
                .await?;
            println!("{}", output::render(&response, json, format_reports_table)?);
        }
        ReconciliationCommands::Report { report_id, json } => {
            let response: ReportDetailResponse = client
                .get(&format!("/admin/reconciliation/reports/{report_id}"))
                .await?;
            println!("{}", output::render(&response, json, format_report_table)?);
        }
        ReconciliationCommands::Run {
            account,
            period_hours,
            json,
        } => {
            let response: RunResponse = client
                .post_json(
                    "/admin/reconciliation/run",
                    &RunRequest {
                        account: &account,
                        period_hours: Some(period_hours),
                    },
                )
                .await?;
            println!("{}", output::render(&response, json, format_run_table)?);
        }
    }

    Ok(())
}

async fn handle_transactions(client: &ApiClient, command: TransactionsCommands) -> Result<()> {
    match command {
        TransactionsCommands::Export {
            format,
            from,
            to,
            status,
            asset_code,
            output,
        } => {
            let mut query = vec![("format", format)];
            push_optional_query(&mut query, "from", from);
            push_optional_query(&mut query, "to", to);
            push_optional_query(&mut query, "status", status);
            push_optional_query(&mut query, "asset_code", asset_code);

            let bytes = client.get_bytes("/export", &query).await?;
            if let Some(path) = output {
                std::fs::write(&path, bytes)?;
                println!("Exported to {path}");
            } else {
                println!("{}", String::from_utf8(bytes)?);
            }
        }
    }

    Ok(())
}

async fn handle_settlements(client: &ApiClient, command: SettlementsCommands) -> Result<()> {
    match command {
        SettlementsCommands::List {
            cursor,
            limit,
            direction,
            format,
        } => {
            let mut query = vec![("limit", limit.to_string()), ("direction", direction)];
            push_optional_query(&mut query, "cursor", cursor);
            let query = query
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>()
                .join("&");
            let response: serde_json::Value = client.get(&format!("/settlements?{query}")).await?;
            println!(
                "{}",
                Formatter::format_json_output(&response, OutputFormat::from_str(&format))?
            );
        }
        SettlementsCommands::Get {
            settlement_id,
            format,
        } => {
            let response: serde_json::Value =
                client.get(&format!("/settlements/{settlement_id}")).await?;
            println!(
                "{}",
                Formatter::format_json_output(&response, OutputFormat::from_str(&format))?
            );
        }
        Commands::Graphql { command } => {
            if let Err(e) = handlers::handle_graphql(command, &config).await {
                eprintln!("error: {}", e);
                std::process::exit(1);
            }
        }
    }

    Ok(())
    }
    Ok(())
}

// ── Events ───────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Serialize)]
struct Event {
    transaction_id: Uuid,
    status: String,
    timestamp: String,
    message: Option<String>,
}

// ── Reconnect response types ──────────────────────────────────────────────────
// Mirrors src/handlers/reconnection.rs ReconnectionResponse / ReconnectStatus.

/// Top-level response for `POST /reconnect` and `GET /reconnect/status`.
/// The server serialises as `{"type": "reconnect", ...}` or `{"type": "error", ...}`.
#[derive(Debug, Deserialize, Serialize)]
struct ReconnectResponse {
    /// Discriminant: `"reconnect"` or `"error"`.
    #[serde(rename = "type")]
    kind: String,
    /// Embedded status payload (present when `kind == "reconnect"`).
    status: Option<ReconnectStatusPayload>,
    /// Suggested backoff in seconds before the next attempt.
    backoff_seconds: Option<u64>,
    /// Whether a full state resync is required after reconnecting.
    requires_resync: Option<bool>,
    /// Human-readable error message (present when `kind == "error"`).
    message: Option<String>,
}

/// Inner status object: mirrors `ReconnectStatus` in the server handler.
/// Tagged with `"status"` field, values snake_cased.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ReconnectStatusPayload {
    /// Reconnect immediately; `session_id` is the new opaque cursor.
    Ready { session_id: String },
    /// Rate-limited; wait `wait_seconds` before retrying.
    RetryAfter { wait_seconds: u64 },
    /// Previous session has expired — start a fresh connection.
    SessionExpired,
    /// Supplied token/cursor is not a valid session identifier.
    InvalidToken,
}

impl ReconnectStatusPayload {
    fn label(&self) -> &str {
        match self {
            ReconnectStatusPayload::Ready { .. } => "ready",
            ReconnectStatusPayload::RetryAfter { .. } => "retry_after",
            ReconnectStatusPayload::SessionExpired => "session_expired",
            ReconnectStatusPayload::InvalidToken => "invalid_token",
        }
    }

    fn session_id(&self) -> Option<&str> {
        match self {
            ReconnectStatusPayload::Ready { session_id } => Some(session_id.as_str()),
            _ => None,
        }
    }
}

// ── Events handler ────────────────────────────────────────────────────────────

async fn handle_events(
    client: &reqwest::Client,
    base_url: &str,
    command: EventsCommands,
) -> Result<()> {
    match command {
        EventsCommands::Watch { json } => {
            let url = format!("{base_url}/events");
            let events = send_json_request::<Vec<Event>>(client.get(url)).await?;
            println!("{}", output::render(&events, json, format_events_table)?);
        }

        // ── synapse admin events reconnect --cursor <CURSOR> ──────────────
        EventsCommands::Reconnect { cursor, json } => {
            let url = format!("{base_url}/reconnect");
            let body = serde_json::json!({ "session_id": cursor });
            let response =
                send_json_request::<ReconnectResponse>(client.post(url).json(&body)).await?;
            println!("{}", output::render(&response, json, format_reconnect_table)?);
        }

        // ── synapse admin events reconnect-status [--cursor <CURSOR>] ─────
        //
        // Edge case: omitting --cursor is always valid; the server creates a
        // fresh session and returns `status: ready`.
        EventsCommands::ReconnectStatus { cursor, json } => {
            let url = format!("{base_url}/reconnect/status");
            let mut req = client.get(&url);
            if let Some(ref token) = cursor {
                req = req.query(&[("token", token.as_str())]);
            }
            let response = send_json_request::<ReconnectResponse>(req).await?;
            println!("{}", output::render(&response, json, format_reconnect_table)?);
        }
    }
    Ok(())
}

fn format_events_table(events: &Vec<Event>) -> String {
    if events.is_empty() {
        return "No events".to_string();
    }

    let mut lines = vec![
        "TRANSACTION ID | STATUS | TIMESTAMP | MESSAGE".to_string(),
        "-------------- | ------ | --------- | -------".to_string(),
    ];

    for ev in events {
        lines.push(format!(
            "{} | {} | {} | {}",
            ev.transaction_id,
            ev.status,
            ev.timestamp,
            ev.message.as_deref().unwrap_or("-"),
        ));
    }

    lines.join("\n")
}

fn format_reconnect_table(response: &ReconnectResponse) -> String {
    let status_label = response
        .status
        .as_ref()
        .map(|s| s.label())
        .unwrap_or("-");

    let session_id = response
        .status
        .as_ref()
        .and_then(|s| s.session_id())
        .unwrap_or("-");

    let backoff = response
        .backoff_seconds
        .map(|b| b.to_string())
        .unwrap_or_else(|| "-".to_string());

    let resync = response
        .requires_resync
        .map(|r| r.to_string())
        .unwrap_or_else(|| "-".to_string());

    let msg = response.message.as_deref().unwrap_or("-");

    [
        format!("TYPE             {}", response.kind),
        format!("STATUS           {status_label}"),
        format!("SESSION ID       {session_id}"),
        format!("BACKOFF (s)      {backoff}"),
        format!("REQUIRES RESYNC  {resync}"),
        format!("MESSAGE          {msg}"),
    ]
    .join("\n")
}

// ─────────────────────────────────────────────────────────────────────────────

async fn send_json_request<T>(request: reqwest::RequestBuilder) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let response = request.send().await.context("request failed")?;
    let status = response.status();
    let body = response.text().await.context("failed to read response body")?;
fn push_optional_query(
    query: &mut Vec<(&'static str, String)>,
    key: &'static str,
    value: Option<String>,
) {
    if let Some(value) = value {
        query.push((key, value));
    }
}

fn print_completions(shell: &str) -> Result<()> {
    match shell {
        "bash" => println!("_synapse() {{\n    :\n}}\ncomplete -F _synapse synapse"),
        "zsh" => println!("#compdef synapse\ncompdef _synapse synapse\n_synapse() {{\n    :\n}}"),
        "fish" => println!("complete -c synapse -f"),
        _ => bail!("Unsupported shell: {shell}"),
    }

    Ok(())
}

// ── Table formatters ──────────────────────────────────────────────────────────

fn format_reports_table(response: &ListReportsResponse) -> String {
    let mut lines = vec![format!(
        "Reports: {} total (showing {} from offset {}, limit {})",
        response.total,
        response.reports.len(),
        response.offset,
        response.limit
    )];

    if response.reports.is_empty() {
        lines.push("No reconciliation reports found".to_string());
        return lines.join("\n");
    }

    lines.push(
        "ID | Generated | Period Start | Period End | DB | Chain | Discrepancies".to_string(),
    );
    lines.push(
        "-- | --------- | ------------ | ---------- | -- | ----- | -------------".to_string(),
    );

    for report in &response.reports {
        lines.push(format!(
            "{} | {} | {} | {} | {} | {} | {}",
            report.id,
            report.generated_at,
            report.period_start,
            report.period_end,
            report.total_db_transactions,
            report.total_chain_payments,
            yes_no(report.has_discrepancies)
        ));
    }

    lines.join("\n")
}

fn format_report_table(report: &ReportDetailResponse) -> String {
    let mut lines = vec![
        format!("Report ID: {}", report.id),
        format!("Generated: {}", report.generated_at),
        format!("Period: {} to {}", report.period_start, report.period_end),
        String::new(),
        "Summary:".to_string(),
        format!(
            "  Database transactions: {}",
            report.summary.total_db_transactions
        ),
        format!("  Chain payments: {}", report.summary.total_chain_payments),
        format!(
            "  Missing on chain: {}",
            report.summary.missing_on_chain_count
        ),
        format!(
            "  Orphaned payments: {}",
            report.summary.orphaned_payments_count
        ),
        format!(
            "  Amount mismatches: {}",
            report.summary.amount_mismatches_count
        ),
        format!(
            "  Has discrepancies: {}",
            yes_no(report.summary.has_discrepancies)
        ),
    ];

    if report.missing_on_chain.is_empty()
        && report.orphaned_payments.is_empty()
        && report.amount_mismatches.is_empty()
    {
        lines.push(String::new());
        lines.push("No discrepancies found".to_string());
    }

    lines.join("\n")
}

fn format_run_table(response: &RunResponse) -> String {
    let report = &response.report;
    [
        response.message.clone(),
        String::new(),
        format!("Report ID: {}", report.id),
        format!("Generated: {}", report.generated_at),
        format!("Period: {} to {}", report.period_start, report.period_end),
        String::new(),
        "Summary:".to_string(),
        format!("  Database transactions: {}", report.total_db_transactions),
        format!("  Chain payments: {}", report.total_chain_payments),
        format!("  Missing on chain: {}", report.missing_on_chain_count),
        format!("  Orphaned payments: {}", report.orphaned_payments_count),
        format!("  Amount mismatches: {}", report.amount_mismatches_count),
        format!("  Has discrepancies: {}", yes_no(report.has_discrepancies)),
    ]
    .join("\n")
}

fn format_locks_table(response: &ListLocksResponse) -> String {
    let mut lines = vec![format!(
        "Active locks: {} total ({} overdue)",
        response.total, response.overdue
    )];

    if response.active_locks.is_empty() {
        lines.push("No active locks found".to_string());
        return lines.join("\n");
    }

    lines.push("Resource | Token | Acquired At | TTL | Expected Duration | Overdue".to_string());
    lines.push("-------- | ----- | ----------- | --- | ----------------- | -------".to_string());

    for lock in &response.active_locks {
        lines.push(format!(
            "{} | {} | {} | {} | {} | {}",
            lock.resource,
            lock.token,
            lock.acquired_at,
            lock.ttl_secs,
            lock.expected_duration_secs,
            yes_no(lock.overdue)
        ));
    }

    lines.join("\n")
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
    let mut lines = vec![
        "ID | URL | Enabled | Success Rate | Deliveries | Last Success".to_string(),
        "-- | --- | ------- | ------------ | ---------- | ------------".to_string(),
    ];
    for e in entries {
        lines.push(format!(
            "{} | {} | {} | {:.1}% | {} | {}",
            e.id,
            e.url,
            yes_no(e.enabled),
            e.success_rate,
            e.total_deliveries,
            e.last_success_at.as_deref().unwrap_or("never"),
        ));
    }
    lines.join("\n")
}

fn format_webhook_health_entry(entry: &WebhookHealthEntry) -> String {
    [
        format!("ID: {}", entry.id),
        format!("URL: {}", entry.url),
        format!("Enabled: {}", yes_no(entry.enabled)),
        format!("Success Rate: {:.1}%", entry.success_rate),
        format!("Total Deliveries: {}", entry.total_deliveries),
        format!(
            "Last Success: {}",
            entry.last_success_at.as_deref().unwrap_or("never")
        ),
    ]
    .join("\n")
}

fn format_locks_table(response: &LocksListResponse) -> String {
    let mut lines = vec![format!(
        "Active locks: {} total, {} overdue",
        response.total, response.overdue
    )];

    if response.active_locks.is_empty() {
        lines.push("No locks currently held.".to_string());
        return lines.join("\n");
    }

    lines.push("Resource | Token | Acquired At | TTL (s) | Overdue".to_string());
    lines.push("-------- | ----- | ----------- | ------- | -------".to_string());

    for lock in &response.active_locks {
        lines.push(format!(
            "{} | {} | {} | {} | {}",
            lock.resource,
            lock.token,
            lock.acquired_at,
            lock.ttl_secs,
            yes_no(lock.overdue),
        ));
    }

    lines.join("\n")
}
