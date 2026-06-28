use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

mod output;

#[derive(Parser, Debug)]
#[command(
    name = "synapse",
    version,
    about = "Synapse admin CLI",
    long_about = "Manage Synapse admin reconciliation commands.\n\nUse the nested reconciliation subcommands to list stored reports, inspect a single report, or run a fresh reconciliation against the API.",
    arg_required_else_help = true
)]
struct Cli {
    /// Base URL of the Synapse API.
    #[arg(
        long,
        value_name = "URL",
        default_value = "http://127.0.0.1:3000"
    )]
    base_url: String,

use clap::{Parser, Subcommand};
use synapse_cli::{CliConfig, OutputFormat, SynapseCliClient, Formatter};

mod handlers {
    use crate::{CliConfig, OutputFormat, SynapseCliClient, Formatter};
    use super::{TransactionsCmd, SettlementsCmd};

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
        output_format: OutputFormat,
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
}

#[derive(Parser)]
#[command(name = "synapse")]
#[command(about = "Synapse CLI - Transaction and Settlement Management")]
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
use std::fs;
use synapse_sdk::client::SynapseClient;

mod output;

#[derive(Serialize, Deserialize, Debug, Default)]
struct Config {
    base_url: Option<String>,
    api_key: Option<String>,
}

#[derive(Parser)]
#[command(name = "synapse")]
#[command(about = "Synapse CLI", version)]
struct Args {
    /// API base URL
    #[arg(long, env = "SYNAPSE_BASE_URL")]
    base_url: Option<String>,

    /// API key
    #[arg(long, env = "SYNAPSE_API_KEY")]
    api_key: Option<String>,

    /// Output as JSON
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Health check subcommands
    Health {
        #[command(subcommand)]
        subcommand: HealthCommand,
mod client;
mod formatter;

use clap::{Parser, Subcommand};
use client::{ClientError, SynapseApiClient};
use formatter::Formatter;

#[derive(Parser)]
#[command(name = "synapse")]
#[command(about = "Synapse CLI for interacting with the Synapse API", long_about = None)]
struct Cli {
    #[arg(long, env = "SYNAPSE_BASE_URL", default_value = "http://localhost:8080")]
    base_url: String,

    #[arg(long, env = "SYNAPSE_API_KEY", default_value = "")]
    api_key: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Admin operations exposed by the Synapse API.
    #[command(
        about = "Admin operations",
        long_about = "Admin operations exposed by the Synapse API."
    )]
    Admin(AdminCommands),
}

#[derive(Subcommand, Debug)]
enum AdminCommands {
    /// Reconciliation reports and runs.
    #[command(
        about = "Reconciliation reports",
        long_about = "List, inspect, or run reconciliation reports through the admin API."
    )]
    Reconciliation(ReconciliationCommands),
}

#[derive(Subcommand, Debug)]
enum ReconciliationCommands {
    #[command(
        about = "List reconciliation reports",
        long_about = "List reconciliation reports generated by the admin API.\n\nRequired flags: none.\nOptional flags:\n  --limit <LIMIT>   Maximum number of reports to return (default: 20).\n  --offset <OFFSET>  Number of reports to skip before returning results (default: 0).\n  --json            Print the raw API response as JSON."
    )]
    Reports {
        /// Maximum number of reports to return.
        #[arg(long, value_name = "LIMIT", default_value_t = 20)]
        limit: u32,

        /// Number of reports to skip before returning results.
        #[arg(long, value_name = "OFFSET", default_value_t = 0)]
        offset: u32,

        /// Print the raw API response as JSON.
        #[arg(long)]
        json: bool,
    },

    #[command(
        about = "Show a reconciliation report",
        long_about = "Fetch one reconciliation report by UUID and print the full response body.\n\nRequired flags:\n  <REPORT_ID>       UUID of the report to fetch.\nOptional flags:\n  --json            Print the raw API response as JSON."
    )]
    Report {
        /// UUID of the reconciliation report to fetch.
        #[arg(value_name = "REPORT_ID")]
        report_id: Uuid,

        /// Print the raw API response as JSON.
        #[arg(long)]
        json: bool,
    },

    #[command(
        about = "Run a reconciliation report",
        long_about = "Run a reconciliation pass for one Stellar account and persist the result.\n\nRequired flags:\n  --account <ACCOUNT>      Stellar account to reconcile.\nOptional flags:\n  --period-hours <HOURS>   Hours of history to include (default: 24).\n  --json                   Print the raw API response as JSON."
    )]
    Run {
        /// Stellar account to reconcile.
        #[arg(long, value_name = "ACCOUNT")]
        account: String,

        /// Hours of history to include.
        #[arg(long, value_name = "HOURS")]
        period_hours: Option<u32>,

        /// Print the raw API response as JSON.
        #[arg(long)]
        json: bool,
    },
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
    missing_on_chain: Vec<MissingTransactionOutput>,
    orphaned_payments: Vec<OrphanedPaymentOutput>,
    amount_mismatches: Vec<AmountMismatchOutput>,
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
struct MissingTransactionOutput {
    id: Uuid,
    stellar_account: String,
    amount: String,
    asset_code: String,
    memo: Option<String>,
    created_at: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct OrphanedPaymentOutput {
    payment_id: String,
    from: String,
    to: String,
    amount: String,
    asset_code: String,
    memo: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct AmountMismatchOutput {
    transaction_id: Uuid,
    payment_id: String,
    db_amount: String,
    chain_amount: String,
    memo: Option<String>,
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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = reqwest::Client::new();
    let base_url = cli.base_url.trim_end_matches('/').to_string();

    match cli.command {
        Commands::Admin(admin) => match admin {
            AdminCommands::Reconciliation(command) => {
                handle_reconciliation(&client, &base_url, command).await?
            }
        },
    }

    Ok(())
}

async fn handle_reconciliation(
    client: &reqwest::Client,
    base_url: &str,
    command: ReconciliationCommands,
) -> Result<()> {
    match command {
        ReconciliationCommands::Reports {
            limit,
            offset,
            json,
        } => {
            let url = format!("{base_url}/admin/reconciliation/reports?limit={limit}&offset={offset}");
            let response = send_json_request::<ListReportsResponse>(client.get(url)).await?;
            println!("{}", output::render(&response, json, format_reports_table)?);
        }
        ReconciliationCommands::Report { report_id, json } => {
            let url = format!("{base_url}/admin/reconciliation/reports/{report_id}");
            let response = send_json_request::<ReportDetailResponse>(client.get(url)).await?;
            println!("{}", output::render(&response, json, format_report_table)?);
        }
        ReconciliationCommands::Run {
            account,
            period_hours,
            json,
        } => {
            let url = format!("{base_url}/admin/reconciliation/run");
            let response = send_json_request::<RunResponse>(
                client.post(url).json(&RunRequest {
                    account: &account,
                    period_hours,
                }),
            )
            .await?;
            println!("{}", output::render(&response, json, format_run_table)?);
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
    /// Manage transactions
    Transactions {
        #[command(subcommand)]
        command: TransactionCommand,
    },
}

#[derive(Subcommand)]
enum TransactionsCmd {
    /// Export transactions to CSV or JSON format with optional filters.
    ///
    /// The export command streams raw transaction data without parsing or modification.
    /// Output is written to stdout by default, or to a file with --output.
    ///
    /// All filter flags are optional. When omitted, no filter is applied for that dimension.
    ///
    /// Output format:
    /// - CSV (default): Raw comma-separated values with headers, suitable for spreadsheet import
    /// - JSON: Wrapped in a JSON object with metadata, each row as a JSON object
    ///
    /// Example:
    ///   synapse transactions export
    ///   synapse transactions export --format json --status pending
    ///   synapse transactions export --from 2024-01-01 --to 2024-12-31 --output export.csv
    #[command(about = "Export transactions with optional filters")]
    #[command(long_about = "Export transactions to CSV or JSON format with optional filters.\n\n\
                             The export command streams raw transaction data without parsing.\n\
                             Output is written to stdout by default, or to a file with --output.\n\n\
                             All filter flags are optional:\n\n  \
                             * --format: Export format (csv or json, default: csv)\n  \
                             * --from: Start date filter inclusive (YYYY-MM-DD format)\n  \
                             * --to: End date filter inclusive (YYYY-MM-DD format)\n  \
                             * --status: Filter by transaction status (e.g., pending, completed)\n  \
                             * --asset-code: Filter by asset code (e.g., USD, EUR, USDC)\n  \
                             * --output: Save to file instead of stdout")]
    Export {
        /// Export format: 'csv' (default) or 'json'
        /// CSV output contains headers with raw transaction data suitable for spreadsheet import.
        /// JSON output wraps data in a JSON object with optional metadata.
        #[arg(long, default_value = "csv")]
        format: String,

        /// Start date filter (inclusive). Format: YYYY-MM-DD. Optional.
        /// Only transactions created on or after this date are included.
        #[arg(long)]
        from: Option<String>,

        /// End date filter (inclusive). Format: YYYY-MM-DD. Optional.
        /// Only transactions created on or before this date are included.
        #[arg(long)]
        to: Option<String>,

        /// Filter by transaction status. Optional.
        /// Example values: pending, completed, failed, cancelled.
        /// Only transactions with the specified status are included.
        #[arg(long)]
        status: Option<String>,

        /// Filter by asset code. Optional.
        /// Example values: USD, EUR, USDC, BRL.
        /// Only transactions for the specified asset are included.
        #[arg(long)]
        asset_code: Option<String>,

        /// Output file path. Optional. Default: stdout.
        /// If specified, the export is written to this file instead of stdout.
        #[arg(long)]
        output: Option<String>,
    },
}

#[derive(Subcommand)]
enum SettlementsCmd {
    /// List settlements with cursor-based pagination.
    ///
    /// Retrieves a paginated list of settlements, starting from the most recent by default.
    /// Use cursors to navigate pages - cursors are opaque and provided by the API response.
    ///
    /// Optional pagination flags:
    /// - --cursor: Start from a specific position (obtained from a previous response)
    /// - --limit: Number of results per page (1-100, default 10)
    /// - --direction: forward (default, newest first) or backward (oldest first)
    /// - --format: Output format - table (default) or json
    ///
    /// Example:
    ///   synapse settlements list
    ///   synapse settlements list --limit 50 --format json
    ///   synapse settlements list --cursor <cursor-from-previous-response> --direction backward
    #[command(about = "List settlements with cursor-based pagination")]
    #[command(long_about = "List settlements with cursor-based pagination.\n\n\
                             Retrieves a paginated list of settlements, starting from the most recent.\n\
                             Use cursors to navigate pages - always use the cursor from the API response.\n\n\
                             Optional flags:\n\n  \
                             * --cursor: Start from a specific position (from previous response)\n  \
                             * --limit: Number of results per page (1-100, default: 10)\n  \
                             * --direction: forward (default, newest first) or backward (oldest first)\n  \
                             * --format: Output format - table (default) or json")]
    List {
        /// Pagination cursor from a previous response. Optional.
        /// Cursors are opaque - never construct or modify them manually.
        /// Always obtain cursors from the API response's next_cursor field.
        #[arg(long)]
        cursor: Option<String>,

        /// Number of results per page. Default: 10. Range: 1-100. Optional.
        /// Larger limits retrieve more data in fewer requests but use more memory.
        #[arg(long, default_value = "10")]
        limit: i64,

        /// Pagination direction. Default: forward. Optional.
        /// Use 'forward' to retrieve settlements from newest to oldest (default).
        /// Use 'backward' to retrieve settlements from oldest to newest.
        #[arg(long, default_value = "forward")]
        direction: String,

        /// Output format. Default: table. Optional.
        /// Use 'table' for human-readable columnar output.
        /// Use 'json' for complete JSON structure with all fields.
        #[arg(long, default_value = "table")]
        format: String,
    },

    /// Get a specific settlement by ID.
    ///
    /// Retrieves detailed information about a settlement, including all fields
    /// and metadata. The ID must be a valid UUID.
    ///
    /// Required arguments:
    /// - settlement_id: The settlement UUID (format: xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx)
    ///
    /// Optional flags:
    /// - --format: Output format - table (default) or json
    ///
    /// Example:
    ///   synapse settlements get 550e8400-e29b-41d4-a716-446655440000
    ///   synapse settlements get 550e8400-e29b-41d4-a716-446655440000 --format json
    #[command(about = "Get a specific settlement by ID")]
    #[command(long_about = "Get a specific settlement by ID.\n\n\
                             Retrieves detailed information about a settlement, including all fields.\n\n\
                             Required:\n  \
                             * settlement_id: The settlement UUID (xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx)\n\n\
                             Optional:\n  \
                             * --format: Output format - table (default) or json")]
    Get {
        /// Settlement UUID. Required.
        /// Must be a valid UUID in format: xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx
        settlement_id: String,

        /// Output format. Default: table. Optional.
        /// Use 'table' for human-readable key-value output.
        /// Use 'json' for complete JSON structure with all fields.
enum HealthCommand {
    /// Check if the service is live
    Live,
    /// Check if the service is ready
    Ready,
    /// General health check
    Check,
    /// Get health errors
    Errors,
}

fn load_config() -> Config {
    let config_path = match directories::ProjectDirs::from("", "", "synapse-cli") {
        Some(dirs) => dirs.config_dir().join("config.toml"),
        None => return Config::default(),
    };

    if !config_path.exists() {
        return Config::default();
    }

    match fs::read_to_string(&config_path) {
        Ok(content) => match toml::from_str(&content) {
            Ok(config) => config,
            Err(_) => Config::default(),
        },
        Err(_) => Config::default(),
    }
}

impl Args {
    fn resolve_base_url(&self, config: &Config) -> Option<&str> {
        self.base_url
            .as_deref()
            .or_else(|| config.base_url.as_deref())
    }

    fn resolve_api_key(&self, config: &Config) -> Option<&str> {
        self.api_key
            .as_deref()
            .or_else(|| config.api_key.as_deref())
    }
enum TransactionCommand {
    #[command(about = "Fetch a single transaction by its UUID",
              long_about = "Fetch a single transaction by its UUID.\n\n\
                            Exit codes:\n  \
                            0 - Success\n  \
                            1 - Transaction not found or other error\n\n\
                            Output formats:\n  \
                            table - Human-readable table (default)\n  \
                            json - Pretty-printed JSON\n\n\
                            Not-found errors (HTTP 404) are surfaced as exit code 1 with message \
                            'transaction not found: <error message>', distinguishing them from other failure modes.")]
    Get {
        /// Transaction ID (UUID)
        id: String,
        /// Output format: 'table' (default) or 'json'
        #[arg(long, default_value = "table")]
        format: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("synapse_cli=info".parse()?),
        )
        .init();

    let cli = Cli::parse();

    let mut config = CliConfig::from_env()?;
    if let Some(url) = cli.url {
        config.base_url = url;
    }
    if let Some(api_key) = cli.api_key {
        config.api_key = Some(api_key);
    }

    let output_format = OutputFormat::from_str(&cli.format);

    match cli.command {
        Commands::Transactions { command } => {
            handlers::handle_transactions(command, &config, output_format).await?
        }
        Commands::Settlements { command } => {
            handlers::handle_settlements(command, &config, output_format).await?
        }
    }

    Ok(())
}

async fn send_json_request<T>(request: reqwest::RequestBuilder) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let response = request.send().await.context("request failed")?;
    let status = response.status();
    let body = response.text().await.context("failed to read response body")?;

    if !status.is_success() {
        bail!("server returned {status}: {body}");
    }

    serde_json::from_str(&body).context("failed to parse response JSON")
}

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
        format!("  Database transactions: {}", report.summary.total_db_transactions),
        format!("  Chain payments: {}", report.summary.total_chain_payments),
        format!("  Missing on chain: {}", report.summary.missing_on_chain_count),
        format!("  Orphaned payments: {}", report.summary.orphaned_payments_count),
        format!("  Amount mismatches: {}", report.summary.amount_mismatches_count),
        format!("  Has discrepancies: {}", yes_no(report.summary.has_discrepancies)),
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

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
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
async fn main() {
    let args = Args::parse();
    let config = load_config();

    let base_url = match args.resolve_base_url(&config) {
        Some(url) => url,
        None => {
            if args.command.is_some() {
                eprintln!("Error: base_url is required");
                std::process::exit(1);
            }
            return;
        }
    };

    let api_key = match args.resolve_api_key(&config) {
        Some(key) => key,
        None => {
            if args.command.is_some() {
                eprintln!("Error: api_key is required");
                std::process::exit(1);
            }
            return;
        }
    };

    if let Some(Command::Health { subcommand }) = args.command {
        let client = SynapseClient::builder(base_url, api_key).build();
        match subcommand {
            HealthCommand::Live => {
                match client.health().live().await {
                    Ok(status) => output::format_output(status, args.json),
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            HealthCommand::Ready => {
                match client.health().ready().await {
                    Ok(status) => output::format_output(status, args.json),
                    Err(e) => {
                        output::format_output(
                            serde_json::json!({ "error": e.to_string() }),
                            args.json,
                        );
                    }
                }
            }
            HealthCommand::Check => {
                match client.health().health().await {
                    Ok(status) => output::format_output(status, args.json),
                    Err(e) => {
                        eprintln!("Error: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            HealthCommand::Errors => {
                match client.health().errors().await {
                    Ok(errors) => output::format_output(errors, args.json),
                    Err(e) => {
                        eprintln!("Error: {}", e);
    let cli = Cli::parse();

    match cli.command {
        Commands::Transactions { command } => match command {
            TransactionCommand::Get { id, format } => {
                let client = SynapseApiClient::new(cli.base_url, cli.api_key);
                match client.get_transaction(&id).await {
                    Ok(tx) => {
                        let output = Formatter::format(&format, &tx);
                        println!("{}", output);
                        std::process::exit(0);
                    }
                    Err(ClientError::NotFound(msg)) => {
                        eprintln!("transaction not found: {}", msg);
                        std::process::exit(1);
                    }
                    Err(e) => {
                        eprintln!("error: {}", e);
                        std::process::exit(1);
                    }
                }
            }
        }
        },
    }
}
