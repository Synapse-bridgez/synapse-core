//! Synapse CLI binary.
//!
//! # Shell completions  (#897)
//!
//! The `synapse completions <shell>` sub-command generates real shell
//! completion scripts by delegating to [`clap_complete::generate`].  The
//! completions enumerate every sub-command, flag, and argument that clap
//! knows about from the [`Cli`] struct — including `admin`, `events`,
//! `health`, `stats`, `settlements`, `transactions`, `graphql`, and
//! `completions` itself, together with global flags such as `--base-url`,
//! `--api-key`, and `--json`.
//!
//! Previous versions of this file hand-wrote stub functions whose bodies were
//! a literal no-op (`:`), giving the user tab completion that did nothing.
//! That stub is gone; `clap_complete::generate` is the single source of truth.

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::{generate, Shell};
use std::io;

mod commands;

// ---------------------------------------------------------------------------
// Global flags shared across every sub-command
// ---------------------------------------------------------------------------

/// Synapse command-line interface.
#[derive(Parser, Debug)]
#[command(
    name = "synapse",
    version,
    author,
    about = "Interact with the Synapse API from the command line",
    long_about = None,
)]
pub struct Cli {
    /// Synapse API base URL.  Can also be set via `SYNAPSE_BASE_URL`.
    #[arg(long, global = true, env = "SYNAPSE_BASE_URL")]
    pub base_url: Option<String>,

    /// API key for authentication.  Can also be set via `SYNAPSE_API_KEY`.
    #[arg(long, global = true, env = "SYNAPSE_API_KEY")]
    pub api_key: Option<String>,

    /// Emit raw JSON output instead of human-readable tables.
    #[arg(long, global = true, default_value = "false")]
    pub json: bool,

    #[command(subcommand)]
    pub command: Commands,
}

// ---------------------------------------------------------------------------
// Top-level sub-commands
// ---------------------------------------------------------------------------

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Manage admin operations (event reconnection, system maintenance, …).
    Admin {
        #[command(subcommand)]
        subcommand: AdminSubcommand,
    },

    /// Stream live transaction-status events over WebSocket.
    Events(commands::EventsCmd),

    /// Check the health and readiness of the Synapse service.
    Health,

    /// Fetch runtime statistics (status counts, daily totals, cache metrics, …).
    Stats {
        #[command(subcommand)]
        subcommand: StatsSubcommand,
    },

    /// List or get settlement records.
    Settlements {
        #[command(subcommand)]
        subcommand: SettlementsSubcommand,
    },

    /// Query or search transactions.
    Transactions {
        #[command(subcommand)]
        subcommand: TransactionsSubcommand,
    },

    /// Execute a raw GraphQL query against the Synapse API.
    Graphql {
        /// GraphQL query string.
        #[arg(value_name = "QUERY")]
        query: String,
    },

    /// Print a shell-completion script to stdout and exit.
    ///
    /// Source the output in your shell config to enable tab-completion for
    /// every `synapse` sub-command, flag, and argument.
    ///
    /// # Examples
    ///
    /// ```bash
    /// # Bash — add to ~/.bashrc
    /// eval "$(synapse completions bash)"
    ///
    /// # Zsh — add to ~/.zshrc
    /// eval "$(synapse completions zsh)"
    ///
    /// # Fish — save to the completions directory
    /// synapse completions fish > ~/.config/fish/completions/synapse.fish
    /// ```
    Completions {
        /// Target shell: `bash`, `zsh`, or `fish`.
        #[arg(value_enum)]
        shell: Shell,
    },
}

// ---------------------------------------------------------------------------
// Admin sub-commands
// ---------------------------------------------------------------------------

#[derive(Subcommand, Debug)]
pub enum AdminSubcommand {
    /// Trigger a WebSocket reconnection on the server.
    Reconnect {
        /// Admin API key (overrides `--api-key`).
        #[arg(long, env = "SYNAPSE_ADMIN_KEY")]
        admin_key: Option<String>,
    },
    /// Query the current reconnection status.
    ReconnectStatus {
        /// Admin API key (overrides `--api-key`).
        #[arg(long, env = "SYNAPSE_ADMIN_KEY")]
        admin_key: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// Stats sub-commands
// ---------------------------------------------------------------------------

#[derive(Subcommand, Debug)]
pub enum StatsSubcommand {
    /// Transaction counts grouped by status.
    Status,
    /// Daily transaction totals for the past N days.
    Daily {
        /// Number of days to include (1–365, default 7).
        #[arg(short, long, default_value = "7")]
        days: i32,
    },
    /// Per-asset transaction statistics.
    Assets,
    /// Combined query-cache and idempotency-cache metrics from `/cache/metrics`.
    Cache,
}

// ---------------------------------------------------------------------------
// Settlements sub-commands
// ---------------------------------------------------------------------------

#[derive(Subcommand, Debug)]
pub enum SettlementsSubcommand {
    /// List all settlements.
    List,
    /// Get a single settlement by ID.
    Get {
        /// Settlement UUID.
        #[arg(value_name = "ID")]
        id: String,
    },
}

// ---------------------------------------------------------------------------
// Transactions sub-commands
// ---------------------------------------------------------------------------

#[derive(Subcommand, Debug)]
pub enum TransactionsSubcommand {
    /// List transactions with optional cursor-based pagination.
    List {
        /// Maximum records per page (default 25).
        #[arg(short, long)]
        limit: Option<i64>,
        /// Pagination cursor from a previous response.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Get a single transaction by UUID.
    Get {
        /// Transaction UUID.
        #[arg(value_name = "ID")]
        id: String,
    },
    /// Search transactions by status, asset code, amount range, or date range.
    Search {
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        asset_code: Option<String>,
        #[arg(long)]
        min_amount: Option<String>,
        #[arg(long)]
        max_amount: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Completions { shell } => {
            // Generate real completions from the live Cli struct via clap_complete.
            // Previously this printed a hand-written no-op stub; now clap_complete
            // enumerates every sub-command and flag from the Cli definition.
            print_completions(shell);
        }
        Commands::Events(cmd) => {
            cmd.run().await?;
        }
        Commands::Health => {
            let base_url = require_base_url(&cli.base_url)?;
            let client = reqwest::Client::new();
            let resp = client
                .get(format!("{}/health", base_url))
                .send()
                .await?;
            println!("{}", resp.text().await?);
        }
        Commands::Stats { subcommand } => {
            run_stats(subcommand, &cli.base_url, &cli.api_key).await?;
        }
        Commands::Transactions { subcommand } => {
            run_transactions(subcommand, &cli.base_url, &cli.api_key).await?;
        }
        Commands::Settlements { subcommand } => {
            run_settlements(subcommand, &cli.base_url, &cli.api_key).await?;
        }
        Commands::Graphql { query } => {
            let base_url = require_base_url(&cli.base_url)?;
            let api_key = require_api_key(&cli.api_key)?;
            let client = reqwest::Client::new();
            let body = serde_json::json!({ "query": query });
            let resp = client
                .post(format!("{}/graphql", base_url))
                .header("X-API-Key", api_key)
                .json(&body)
                .send()
                .await?;
            let json: serde_json::Value = resp.json().await?;
            println!("{}", serde_json::to_string_pretty(&json)?);
        }
        Commands::Admin { subcommand } => {
            run_admin(subcommand, &cli.base_url, &cli.api_key).await?;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Completion generation — real output via clap_complete  (#897)
// ---------------------------------------------------------------------------

/// Write a real shell-completion script to stdout.
///
/// Uses [`clap_complete::generate`] to derive completions directly from the
/// [`Cli`] struct so that every sub-command and flag is always in sync with
/// the source code.  There is no hand-written stub; `clap_complete` is the
/// sole source of truth.
fn print_completions(shell: Shell) {
    let mut cmd = Cli::command();
    generate(shell, &mut cmd, "synapse", &mut io::stdout());
}

// ---------------------------------------------------------------------------
// Command runners (thin: delegate to reqwest, print JSON)
// ---------------------------------------------------------------------------

async fn run_stats(
    sub: StatsSubcommand,
    base_url: &Option<String>,
    api_key: &Option<String>,
) -> Result<()> {
    let base = require_base_url(base_url)?;
    let key = require_api_key(api_key)?;
    let client = reqwest::Client::new();

    let path = match &sub {
        StatsSubcommand::Status => "/stats/status".to_string(),
        StatsSubcommand::Daily { days } => format!("/stats/daily?days={days}"),
        StatsSubcommand::Assets => "/stats/assets".to_string(),
        StatsSubcommand::Cache => "/cache/metrics".to_string(),
    };

    let resp = client
        .get(format!("{base}{path}"))
        .header("Authorization", format!("Bearer {key}"))
        .send()
        .await?;
    let json: serde_json::Value = resp.json().await?;
    println!("{}", serde_json::to_string_pretty(&json)?);
    Ok(())
}

async fn run_transactions(
    sub: TransactionsSubcommand,
    base_url: &Option<String>,
    api_key: &Option<String>,
) -> Result<()> {
    let base = require_base_url(base_url)?;
    let key = require_api_key(api_key)?;
    let client = reqwest::Client::new();

    let (method, path) = match &sub {
        TransactionsSubcommand::List { limit, cursor } => {
            let mut qs = Vec::new();
            if let Some(l) = limit {
                qs.push(format!("limit={l}"));
            }
            if let Some(c) = cursor {
                qs.push(format!("cursor={c}"));
            }
            let q = if qs.is_empty() {
                String::new()
            } else {
                format!("?{}", qs.join("&"))
            };
            ("GET", format!("/transactions{q}"))
        }
        TransactionsSubcommand::Get { id } => ("GET", format!("/transactions/{id}")),
        TransactionsSubcommand::Search {
            status,
            asset_code,
            min_amount,
            max_amount,
        } => {
            let mut qs = Vec::new();
            if let Some(s) = status {
                qs.push(format!("status={s}"));
            }
            if let Some(a) = asset_code {
                qs.push(format!("asset_code={a}"));
            }
            if let Some(mn) = min_amount {
                qs.push(format!("min_amount={mn}"));
            }
            if let Some(mx) = max_amount {
                qs.push(format!("max_amount={mx}"));
            }
            let q = if qs.is_empty() {
                String::new()
            } else {
                format!("?{}", qs.join("&"))
            };
            ("GET", format!("/transactions/search{q}"))
        }
    };

    let builder = if method == "GET" {
        client.get(format!("{base}{path}"))
    } else {
        client.post(format!("{base}{path}"))
    };

    let resp = builder
        .header("X-API-Key", key)
        .send()
        .await?;
    let json: serde_json::Value = resp.json().await?;
    println!("{}", serde_json::to_string_pretty(&json)?);
    Ok(())
}

async fn run_settlements(
    sub: SettlementsSubcommand,
    base_url: &Option<String>,
    api_key: &Option<String>,
) -> Result<()> {
    let base = require_base_url(base_url)?;
    let key = require_api_key(api_key)?;
    let client = reqwest::Client::new();

    let path = match &sub {
        SettlementsSubcommand::List => "/settlements".to_string(),
        SettlementsSubcommand::Get { id } => format!("/settlements/{id}"),
    };

    let resp = client
        .get(format!("{base}{path}"))
        .header("X-API-Key", key)
        .send()
        .await?;
    let json: serde_json::Value = resp.json().await?;
    println!("{}", serde_json::to_string_pretty(&json)?);
    Ok(())
}

async fn run_admin(
    sub: AdminSubcommand,
    base_url: &Option<String>,
    api_key: &Option<String>,
) -> Result<()> {
    let base = require_base_url(base_url)?;
    let key = require_api_key(api_key)?;
    let client = reqwest::Client::new();

    let (method, path) = match &sub {
        AdminSubcommand::Reconnect { admin_key } => {
            let key_override = admin_key.as_deref().unwrap_or(key);
            let resp = client
                .post(format!("{base}/reconnect"))
                .header("Authorization", format!("Bearer {key_override}"))
                .send()
                .await?;
            let json: serde_json::Value = resp.json().await?;
            println!("{}", serde_json::to_string_pretty(&json)?);
            return Ok(());
        }
        AdminSubcommand::ReconnectStatus { admin_key } => {
            let key_override = admin_key.as_deref().unwrap_or(key);
            let resp = client
                .get(format!("{base}/reconnect/status"))
                .header("Authorization", format!("Bearer {key_override}"))
                .send()
                .await?;
            let json: serde_json::Value = resp.json().await?;
            println!("{}", serde_json::to_string_pretty(&json)?);
            return Ok(());
        }
    };

    // Unreachable fallthrough — kept to satisfy the pattern-match exhaustion check.
    #[allow(unreachable_code)]
    {
        let _ = (method, path);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn require_base_url(opt: &Option<String>) -> Result<&str> {
    opt.as_deref()
        .ok_or_else(|| anyhow::anyhow!("--base-url (or SYNAPSE_BASE_URL) is required"))
}

fn require_api_key(opt: &Option<String>) -> Result<&str> {
    opt.as_deref()
        .ok_or_else(|| anyhow::anyhow!("--api-key (or SYNAPSE_API_KEY) is required"))
}
