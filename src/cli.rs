use crate::config::Config;
use clap::{Parser, Subcommand};
use sqlx::PgPool;
use uuid::Uuid;

// Re-export handler functions used in tests and from main dispatch.
// `reqwest` and `serde_json` are regular [dependencies] so they are available
// to every module in the binary crate without an explicit `extern crate`.

#[derive(Parser)]
#[command(name = "synapse-core")]
#[command(about = "Synapse Core - Fiat Gateway Callback Processor", long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Start the HTTP server (default)
    Serve,

    /// Transaction management commands
    #[command(subcommand)]
    Tx(TxCommands),

    /// Settlement management commands
    #[command(subcommand)]
    Settlements(SettlementsCommands),

    /// Database management commands
    #[command(subcommand)]
    Db(DbCommands),

    /// Backup management commands
    #[command(subcommand)]
    Backup(BackupCommands),

    /// Configuration validation
    Config,

    /// Query live transaction statistics from the running server
    ///
    /// Fetches real-time metrics from the Synapse Core API.
    /// The server must be running and reachable at the given --url.
    ///
    /// Available sub-commands:
    ///   status  – transaction counts grouped by status (pending/processing/completed/failed)
    ///   daily   – daily totals (amount + tx count) over a rolling window
    ///   assets  – per-asset volume, transaction count, and average amount
    ///   cache   – internal query-cache and idempotency-cache hit/miss metrics
    #[command(subcommand)]
    Stats(StatsCommands),

    /// Execute a GraphQL query against the Synapse Core API
    ///
    /// Sends a raw GraphQL document to POST /graphql and pretty-prints
    /// the JSON response.  Supports both queries and mutations.
    ///
    /// GraphQL errors (HTTP 200 with an "errors" array) are surfaced
    /// distinctly from transport/network failures so you can tell the
    /// difference at a glance.
    #[command(subcommand)]
    Graphql(GraphqlCommands),
}

#[derive(Subcommand)]
pub enum TxCommands {
    /// Force complete a transaction by ID
    ForceComplete {
        /// Transaction UUID
        #[arg(value_name = "TX_ID")]
        tx_id: Uuid,
    },

    #[command(
        long_about = "List transactions with cursor-based pagination and optional date filters.

All flags are optional. Cursors are opaque — always use next_cursor from previous response.
Invalid or expired cursors return an error and must not be retried as-is.

Examples:
  synapse-core tx list --limit 50
  synapse-core tx list --from-date 2024-01-01T00:00:00Z --to-date 2024-02-01T00:00:00Z
  synapse-core tx list --cursor <cursor> --format json"
    )]
    List {
        /// Opaque pagination cursor (use next_cursor from previous response)
        #[arg(long)]
        cursor: Option<String>,

        /// Maximum records per page (server default: 25, max: 100)
        #[arg(long, short = 'l')]
        limit: Option<i64>,

        /// Inclusive ISO 8601 date range start (e.g., 2024-01-01T00:00:00Z)
        #[arg(long)]
        from_date: Option<String>,

        /// Exclusive ISO 8601 date range end (e.g., 2024-02-01T00:00:00Z)
        #[arg(long)]
        to_date: Option<String>,

        /// Output format (json or table; default: table)
        #[arg(long, default_value = "table")]
        format: String,
    },

    /// Run reconciliation report
    Reconcile {
        /// Stellar account to reconcile
        #[arg(value_name = "ACCOUNT")]
        account: String,

        /// Start date (ISO 8601 format)
        #[arg(long)]
        start: String,

        /// End date (ISO 8601 format)
        #[arg(long)]
        end: String,

        /// Output format (json or text)
        #[arg(long, default_value = "text")]
        format: String,
    },

    /// Search transactions by filters
    ///
    /// Search for transactions using optional filters like status, asset code, amount range,
    /// date range, and Stellar account. Results are paginated with cursor-based navigation.
    ///
    /// # Examples
    ///
    /// Search all pending transactions:
    /// ```sh
    /// synapse-core tx search --status pending
    /// ```
    ///
    /// Search completed USD transactions with amount between 100 and 500:
    /// ```sh
    /// synapse-core tx search --status completed --asset-code USD --min-amount 100.00 --max-amount 500.00
    /// ```
    ///
    /// Search transactions in a date range:
    /// ```sh
    /// synapse-core tx search --from 2024-01-01T00:00:00Z --to 2024-01-31T23:59:59Z
    /// ```
    ///
    /// Get results as JSON:
    /// ```sh
    /// synapse-core tx search --status completed --format json
    /// ```
    ///
    /// Use pagination cursor for next page:
    /// ```sh
    /// synapse-core tx search --cursor <cursor-from-previous-response>
    /// ```
    Search {
        /// Transaction status (pending, processing, completed, failed)
        #[arg(long)]
        status: Option<String>,

        /// Asset code (e.g., USD)
        #[arg(long)]
        asset_code: Option<String>,

        /// Minimum amount (inclusive)
        #[arg(long)]
        min_amount: Option<String>,

        /// Maximum amount (inclusive)
        #[arg(long)]
        max_amount: Option<String>,

        /// Start date (ISO 8601 format, inclusive)
        #[arg(long)]
        from: Option<String>,

        /// End date (ISO 8601 format, exclusive)
        #[arg(long)]
        to: Option<String>,

        /// Stellar account to filter by
        #[arg(long)]
        stellar_account: Option<String>,

        /// Pagination cursor
        #[arg(long)]
        cursor: Option<String>,

        /// Maximum records per page
        #[arg(long, default_value = "25")]
        limit: i64,

        /// Output format (json or table)
        #[arg(long, default_value = "table")]
        format: String,
    },
}

#[derive(Subcommand)]
pub enum SettlementsCommands {
    /// List all settlements
    List {
        /// Output format (json or table)
        #[arg(long, default_value = "table")]
        format: String,
    },

    /// Get a specific settlement by ID
    Get {
        /// Settlement UUID
        #[arg(value_name = "SETTLEMENT_ID")]
        id: String,

        /// Output format (json or table)
        #[arg(long, default_value = "table")]
        format: String,
    },
}

#[derive(Subcommand)]
pub enum DbCommands {
    /// Run database migrations
    Migrate,
}

#[derive(Subcommand)]
pub enum BackupCommands {
    /// Create a new backup
    Run {
        /// Backup type (hourly, daily, monthly)
        #[arg(short, long, default_value = "hourly")]
        backup_type: String,
    },

    /// List all available backups
    List,

    /// Restore from a backup
    Restore {
        /// Backup filename to restore from
        #[arg(value_name = "FILENAME")]
        filename: String,
    },

    /// Restore to a specific point in time
    RestorePitr {
        /// Target timestamp (ISO 8601 format, e.g., 2026-01-15T10:30:00Z)
        #[arg(long)]
        timestamp: String,
    },

    /// Apply retention policy to clean old backups
    Cleanup,
}

// ─── Stats subcommands ────────────────────────────────────────────────────────

#[derive(Subcommand)]
pub enum StatsCommands {
    /// Show transaction counts grouped by status
    ///
    /// Calls GET /stats/status on the running server and prints a table of
    /// status labels (pending, processing, completed, failed) together with
    /// their current counts.
    ///
    /// Examples:
    ///   # Human-readable table (default)
    ///   synapse-core stats status
    ///
    ///   # JSON output – pipe-friendly
    ///   synapse-core stats status --json
    ///
    ///   # Point at a non-default server
    ///   synapse-core stats status --url http://localhost:4000
    Status {
        /// Base URL of the running Synapse Core server
        ///
        /// The command appends /stats/status to this value.
        /// Defaults to http://localhost:3000.
        #[arg(long, default_value = "http://localhost:3000", env = "SYNAPSE_URL")]
        url: String,

        /// Emit raw JSON instead of a formatted table
        #[arg(long)]
        json: bool,
    },

    /// Show daily transaction totals over a rolling window
    ///
    /// Calls GET /stats/daily?days=N on the running server. For each day in
    /// the window it prints the date, summed amount, and transaction count.
    ///
    /// The --days flag controls the window length (1–365, default 7).
    ///
    /// Examples:
    ///   # Last 7 days (default)
    ///   synapse-core stats daily
    ///
    ///   # Last 30 days as JSON
    ///   synapse-core stats daily --days 30 --json
    Daily {
        /// Base URL of the running Synapse Core server
        ///
        /// The command appends /stats/daily?days=<N> to this value.
        #[arg(long, default_value = "http://localhost:3000", env = "SYNAPSE_URL")]
        url: String,

        /// Number of days to include in the rolling window (1–365)
        ///
        /// Defaults to 7.  Values outside the range 1–365 are rejected
        /// by the server with HTTP 400.
        #[arg(long, default_value_t = 7, value_name = "N")]
        days: i32,

        /// Emit raw JSON instead of a formatted table
        #[arg(long)]
        json: bool,
    },

    /// Show per-asset volume, transaction count, and average amount
    ///
    /// Calls GET /stats/assets on the running server and prints one row per
    /// registered asset code with its total_amount, tx_count, and avg_amount.
    ///
    /// Examples:
    ///   synapse-core stats assets
    ///   synapse-core stats assets --json
    Assets {
        /// Base URL of the running Synapse Core server
        #[arg(long, default_value = "http://localhost:3000", env = "SYNAPSE_URL")]
        url: String,

        /// Emit raw JSON instead of a formatted table
        #[arg(long)]
        json: bool,
    },

    /// Show internal query-cache and idempotency-cache metrics
    ///
    /// Calls GET /cache/metrics and displays hit/miss counters for both the
    /// query cache (LRU + Redis) and the idempotency layer.  Useful for
    /// diagnosing cache warm-up and contention issues.
    ///
    /// Examples:
    ///   synapse-core stats cache
    ///   synapse-core stats cache --json
    Cache {
        /// Base URL of the running Synapse Core server
        #[arg(long, default_value = "http://localhost:3000", env = "SYNAPSE_URL")]
        url: String,

        /// Emit raw JSON instead of a formatted table
        #[arg(long)]
        json: bool,
    },
}

// ─── GraphQL subcommands ──────────────────────────────────────────────────────

#[derive(Subcommand)]
pub enum GraphqlCommands {
    /// Send a GraphQL query or mutation to the Synapse Core API
    ///
    /// Posts the given GRAPHQL_QUERY document to POST /graphql and
    /// pretty-prints the full JSON response.
    ///
    /// GraphQL-level errors (HTTP 200 with an "errors" array) are printed
    /// to stderr and the process exits with a non-zero status, making them
    /// easy to distinguish from transport/network errors.
    ///
    /// Optional --variables accepts a JSON object string and is forwarded as
    /// the "variables" field in the request body.
    ///
    /// Examples:
    ///   # List recent transactions
    ///   synapse-core graphql query '{ transactions { id status } }'
    ///
    ///   # Look up a specific transaction
    ///   synapse-core graphql query \
    ///     '{ transaction(id: "UUID") { id status amount assetCode } }'
    ///
    ///   # Pass variables
    ///   synapse-core graphql query \
    ///     '{ transactions { id status } }' \
    ///     --variables '{"filter":{"status":"pending"}}'
    ///
    ///   # Force-complete a transaction via mutation
    ///   synapse-core graphql query \
    ///     'mutation { forceCompleteTransaction(id: "UUID") { id status } }'
    ///
    ///   # Point at a non-default server
    ///   synapse-core graphql query '{ transactions { id } }' \
    ///     --url http://staging.example.com:3000
    Query {
        /// GraphQL query or mutation document
        ///
        /// A valid GraphQL document string, e.g.
        /// '{ transactions { id status } }'
        #[arg(value_name = "GRAPHQL_QUERY")]
        query: String,

        /// Optional JSON object passed as the "variables" field
        ///
        /// Must be a valid JSON object, e.g. '{"filter":{"status":"pending"}}'.
        /// Omit this flag when the query has no variable references.
        #[arg(long, value_name = "JSON")]
        variables: Option<String>,

        /// Base URL of the running Synapse Core server
        ///
        /// The command posts to <url>/graphql.
        /// Defaults to http://localhost:3000.
        #[arg(long, default_value = "http://localhost:3000", env = "SYNAPSE_URL")]
        url: String,
    },
}

// ─── Stats handlers ───────────────────────────────────────────────────────────

pub async fn handle_stats_status(base_url: &str, json: bool) -> anyhow::Result<()> {
    let url = format!("{base_url}/stats/status");
    let resp = reqwest::get(&url)
        .await
        .map_err(|e| anyhow::anyhow!("Request failed: {e}"))?;

    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to parse response: {e}"))?;

    if !status.is_success() {
        anyhow::bail!("Server returned HTTP {status}: {body}");
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    // Pretty table
    println!("{:<20} {:>10}", "STATUS", "COUNT");
    println!("{}", "-".repeat(32));
    if let Some(arr) = body.as_array() {
        for row in arr {
            let status = row.get("status").and_then(|v| v.as_str()).unwrap_or("-");
            let count = row.get("count").and_then(|v| v.as_i64()).unwrap_or(0);
            println!("{:<20} {:>10}", status, count);
        }
    }
    Ok(())
}

pub async fn handle_stats_daily(base_url: &str, days: i32, json: bool) -> anyhow::Result<()> {
    let url = format!("{base_url}/stats/daily?days={days}");
    let resp = reqwest::get(&url)
        .await
        .map_err(|e| anyhow::anyhow!("Request failed: {e}"))?;

    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to parse response: {e}"))?;

    if !status.is_success() {
        anyhow::bail!("Server returned HTTP {status}: {body}");
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    println!("{:<12} {:>18} {:>10}", "DATE", "TOTAL AMOUNT", "TX COUNT");
    println!("{}", "-".repeat(42));
    if let Some(arr) = body.as_array() {
        for row in arr {
            let date = row.get("date").and_then(|v| v.as_str()).unwrap_or("-");
            let amount = row
                .get("total_amount")
                .and_then(|v| v.as_str())
                .unwrap_or("0");
            let count = row.get("tx_count").and_then(|v| v.as_i64()).unwrap_or(0);
            println!("{:<12} {:>18} {:>10}", date, amount, count);
        }
    }
    Ok(())
}

pub async fn handle_stats_assets(base_url: &str, json: bool) -> anyhow::Result<()> {
    let url = format!("{base_url}/stats/assets");
    let resp = reqwest::get(&url)
        .await
        .map_err(|e| anyhow::anyhow!("Request failed: {e}"))?;

    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to parse response: {e}"))?;

    if !status.is_success() {
        anyhow::bail!("Server returned HTTP {status}: {body}");
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    println!(
        "{:<12} {:>18} {:>10} {:>18}",
        "ASSET", "TOTAL AMOUNT", "TX COUNT", "AVG AMOUNT"
    );
    println!("{}", "-".repeat(60));
    if let Some(arr) = body.as_array() {
        for row in arr {
            let code = row
                .get("asset_code")
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            let total = row
                .get("total_amount")
                .and_then(|v| v.as_str())
                .unwrap_or("0");
            let count = row.get("tx_count").and_then(|v| v.as_i64()).unwrap_or(0);
            let avg = row
                .get("avg_amount")
                .and_then(|v| v.as_str())
                .unwrap_or("0");
            println!("{:<12} {:>18} {:>10} {:>18}", code, total, count, avg);
        }
    }
    Ok(())
}

pub async fn handle_stats_cache(base_url: &str, json: bool) -> anyhow::Result<()> {
    let url = format!("{base_url}/cache/metrics");
    let resp = reqwest::get(&url)
        .await
        .map_err(|e| anyhow::anyhow!("Request failed: {e}"))?;

    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to parse response: {e}"))?;

    if !status.is_success() {
        anyhow::bail!("Server returned HTTP {status}: {body}");
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }

    // Query cache section
    println!("Query cache:");
    if let Some(qc) = body.get("query_cache") {
        let hits = qc.get("hits").and_then(|v| v.as_u64()).unwrap_or(0);
        let misses = qc.get("misses").and_then(|v| v.as_u64()).unwrap_or(0);
        let hit_rate = qc.get("hit_rate").and_then(|v| v.as_f64()).unwrap_or(0.0);
        println!("  hits:     {hits}");
        println!("  misses:   {misses}");
        println!("  hit_rate: {:.2}%", hit_rate * 100.0);
    }
    println!();
    println!("Idempotency cache:");
    let idem_hits = body
        .get("idempotency_cache_hits")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let idem_misses = body
        .get("idempotency_cache_misses")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let idem_lock = body
        .get("idempotency_lock_acquired")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let idem_contention = body
        .get("idempotency_lock_contention")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let idem_errors = body
        .get("idempotency_errors")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let idem_fallback = body
        .get("idempotency_fallback_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    println!("  cache_hits:      {idem_hits}");
    println!("  cache_misses:    {idem_misses}");
    println!("  lock_acquired:   {idem_lock}");
    println!("  lock_contention: {idem_contention}");
    println!("  errors:          {idem_errors}");
    println!("  fallback_count:  {idem_fallback}");
    Ok(())
}

// ─── GraphQL handler ──────────────────────────────────────────────────────────

pub async fn handle_graphql_query(
    base_url: &str,
    query: &str,
    variables: Option<&str>,
) -> anyhow::Result<()> {
    let url = format!("{base_url}/graphql");

    // Build request body
    let mut body = serde_json::json!({ "query": query });
    if let Some(vars_str) = variables {
        let vars: serde_json::Value = serde_json::from_str(vars_str).map_err(|e| {
            anyhow::anyhow!("--variables must be a valid JSON object, got parse error: {e}")
        })?;
        if !vars.is_object() {
            anyhow::bail!("--variables must be a JSON object, e.g. '{{\"key\": \"value\"}}'");
        }
        body["variables"] = vars;
    }

    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Transport error – could not reach {url}: {e}"))?;

    let http_status = resp.status();
    let resp_body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to decode JSON response: {e}"))?;

    // GraphQL errors come back as HTTP 200 with a top-level "errors" array.
    // Surface these distinctly from HTTP-level failures.
    if let Some(errors) = resp_body.get("errors") {
        if !errors.as_array().map(|a| a.is_empty()).unwrap_or(true) {
            eprintln!("GraphQL errors:");
            eprintln!("{}", serde_json::to_string_pretty(errors)?);
            std::process::exit(1);
        }
    }

    if !http_status.is_success() {
        anyhow::bail!(
            "HTTP {http_status} from server:\n{}",
            serde_json::to_string_pretty(&resp_body)?
        );
    }

    println!("{}", serde_json::to_string_pretty(&resp_body)?);
    Ok(())
}

pub async fn handle_tx_force_complete(pool: &PgPool, tx_id: Uuid) -> anyhow::Result<()> {
    // Get asset_code before update for cache invalidation
    let asset_code: Option<String> =
        sqlx::query_scalar("SELECT asset_code FROM transactions WHERE id = $1")
            .bind(tx_id)
            .fetch_optional(pool)
            .await?;

    let result = sqlx::query(
        "UPDATE transactions SET status = 'completed', updated_at = NOW() WHERE id = $1 RETURNING id"
    )
    .bind(tx_id)
    .fetch_optional(pool)
    .await?;

    match result {
        Some(_) => {
            // Invalidate cache after update
            if let Some(asset) = asset_code {
                crate::db::queries::invalidate_caches_for_asset(&asset).await;
            }

            tracing::info!("Transaction {} marked as completed", tx_id);
            println!("✓ Transaction {tx_id} marked as completed");
            Ok(())
        }
        None => {
            tracing::warn!("Transaction {} not found", tx_id);
            anyhow::bail!("Transaction {tx_id} not found")
        }
    }
}

pub async fn handle_tx_list(
    config: &Config,
    cursor: Option<String>,
    limit: Option<i64>,
    from_date: Option<String>,
    to_date: Option<String>,
    format: &str,
) -> anyhow::Result<()> {
    let base_url = format!("http://localhost:{}", config.server_port);
    let api_key = std::env::var("SYNAPSE_API_KEY").unwrap_or_else(|_| "dev-key".to_string());

    let client = synapse_sdk::SynapseClient::new(base_url, api_key);
    let params = synapse_sdk::ListParams {
        cursor,
        limit,
        from_date,
        to_date,
    };

    match client.transactions().list(params).await {
        Ok(response) => {
            match format {
                "json" => {
                    let json = serde_json::to_string_pretty(&response)?;
                    println!("{}", json);
                }
                _ => {
                    println!(
                        "{:<36} {:<12} {:<12} {:<15}",
                        "ID", "Status", "Asset", "Amount"
                    );
                    println!("{}", "-".repeat(75));
                    for tx in &response.data {
                        println!(
                            "{:<36} {:<12} {:<12} {:<15}",
                            tx.id, tx.status, tx.asset_code, tx.amount
                        );
                    }
                    println!("\n✓ {} transactions", response.data.len());
                    if let Some(cursor) = response.meta.next_cursor {
                        println!("  Use --cursor {} for next page", cursor);
                    }
                }
            }
            Ok(())
        }
        Err(e) => {
            tracing::error!("Failed to list transactions: {}", e);
            anyhow::bail!("Failed to list transactions: {}", e)
        }
    }
}

pub async fn handle_db_migrate(config: &Config) -> anyhow::Result<()> {
    use sqlx::migrate::Migrator;
    use std::path::Path;

    let pool = crate::db::create_pool(config).await?;
    let migrator = Migrator::new(Path::new("./migrations")).await?;

    tracing::info!("Running database migrations...");
    migrator.run(&pool).await?;

    tracing::info!("Database migrations completed");
    println!("✓ Database migrations completed");

    Ok(())
}

pub fn handle_config_validate(config: &Config) -> anyhow::Result<()> {
    tracing::info!("Validating configuration...");

    println!("Configuration:");
    println!("  Server Port: {}", config.server_port);
    println!("  Database URL: {}", mask_password(&config.database_url));
    println!("  Stellar Horizon URL: {}", config.stellar_horizon_url);

    tracing::info!("Configuration is valid");
    println!("✓ Configuration is valid");

    Ok(())
}

fn mask_password(url: &str) -> String {
    if let Some(at_pos) = url.rfind('@') {
        if let Some(colon_pos) = url[..at_pos].rfind(':') {
            if let Some(slash_pos) = url[..colon_pos].rfind("//") {
                let prefix = &url[..slash_pos + 2];
                let user_start = slash_pos + 2;
                let user = &url[user_start..colon_pos];
                let suffix = &url[at_pos..];
                return format!("{prefix}{user}:****{suffix}");
            }
        }
    }
    url.to_string()
}

pub async fn handle_backup_run(_config: &Config, _backup_type_str: &str) -> anyhow::Result<()> {
    anyhow::bail!("Backup service not yet implemented")
}

pub async fn handle_backup_list(_config: &Config) -> anyhow::Result<()> {
    anyhow::bail!("Backup service not yet implemented")
}

pub async fn handle_backup_restore(_config: &Config, _filename: &str) -> anyhow::Result<()> {
    anyhow::bail!("Backup service not yet implemented")
}

pub async fn handle_backup_cleanup(_config: &Config) -> anyhow::Result<()> {
    anyhow::bail!("Backup service not yet implemented")
}

pub async fn handle_tx_reconcile(
    config: &Config,
    account: &str,
    start: &str,
    end: &str,
    format: &str,
) -> anyhow::Result<()> {
    use crate::services::ReconciliationService;
    use crate::stellar::HorizonClient;
    use chrono::DateTime;

    let pool = crate::db::create_pool(config).await?;
    let horizon_client = HorizonClient::new(config.stellar_horizon_url.clone());
    let service = ReconciliationService::new(horizon_client, pool);

    let start_dt = DateTime::parse_from_rfc3339(start)
        .map_err(|_| {
            anyhow::anyhow!("Invalid start date format. Use ISO 8601 (e.g., 2024-01-01T00:00:00Z)")
        })?
        .with_timezone(&chrono::Utc);

    let end_dt = DateTime::parse_from_rfc3339(end)
        .map_err(|_| {
            anyhow::anyhow!("Invalid end date format. Use ISO 8601 (e.g., 2024-01-31T23:59:59Z)")
        })?
        .with_timezone(&chrono::Utc);

    tracing::info!(
        "Running reconciliation for {} from {} to {}",
        account,
        start_dt,
        end_dt
    );
    let report = service.reconcile(account, start_dt, end_dt).await?;

    match format {
        "json" => {
            let json = serde_json::to_string_pretty(&report)?;
            println!("{json}");
        }
        _ => {
            println!("\n=== Reconciliation Report ===");
            println!("Generated: {}", report.generated_at);
            println!("Period: {} to {}", report.period_start, report.period_end);
            println!("\nSummary:");
            println!("  Database transactions: {}", report.total_db_transactions);
            println!("  Chain payments: {}", report.total_chain_payments);
            println!("  Missing on chain: {}", report.missing_on_chain.len());
            println!("  Orphaned payments: {}", report.orphaned_payments.len());
            println!("  Amount mismatches: {}", report.amount_mismatches.len());

            if !report.missing_on_chain.is_empty() {
                println!("\n⚠️  Missing on Chain:");
                for tx in &report.missing_on_chain {
                    println!(
                        "  - {} | {} {} | memo: {:?}",
                        tx.id, tx.amount, tx.asset_code, tx.memo
                    );
                }
            }

            if !report.orphaned_payments.is_empty() {
                println!("\n⚠️  Orphaned Payments:");
                for payment in &report.orphaned_payments {
                    println!(
                        "  - {} | {} {} | memo: {:?}",
                        payment.payment_id, payment.amount, payment.asset_code, payment.memo
                    );
                }
            }

            if !report.amount_mismatches.is_empty() {
                println!("\n⚠️  Amount Mismatches:");
                for mismatch in &report.amount_mismatches {
                    println!(
                        "  - TX {} | DB: {} | Chain: {} | memo: {:?}",
                        mismatch.transaction_id,
                        mismatch.db_amount,
                        mismatch.chain_amount,
                        mismatch.memo
                    );
                }
            }

            if report.missing_on_chain.is_empty()
                && report.orphaned_payments.is_empty()
                && report.amount_mismatches.is_empty()
            {
                println!("\n✓ No discrepancies found");
            }
        }
    }

    Ok(())
}

#[allow(dead_code)]
pub async fn handle_backup_restore_pitr(
    _config: &Config,
    _timestamp_str: &str,
) -> anyhow::Result<()> {
    anyhow::bail!("PITR restore service not yet implemented")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── handle_graphql_query variable validation (no network) ───────────────

    #[tokio::test]
    async fn test_graphql_query_rejects_invalid_json_variables() {
        let result = handle_graphql_query(
            "http://localhost:9999",
            "{ transactions { id } }",
            Some("not json"),
        )
        .await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("parse error"), "unexpected error: {msg}");
    }

    #[tokio::test]
    async fn test_graphql_query_rejects_non_object_variables() {
        let result = handle_graphql_query(
            "http://localhost:9999",
            "{ transactions { id } }",
            Some("[\"array\"]"),
        )
        .await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("JSON object"), "unexpected error: {msg}");
    }

    // ─── handle_graphql_query with mock server ────────────────────────────────

    #[tokio::test]
    async fn test_graphql_query_success_response() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data":{"transactions":[]}}"#)
            .create_async()
            .await;

        let result = handle_graphql_query(&server.url(), "{ transactions { id } }", None).await;
        mock.assert_async().await;
        assert!(result.is_ok(), "expected success but got: {result:?}");
    }

    #[tokio::test]
    async fn test_graphql_query_empty_errors_array_ok() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data":{},"errors":[]}"#)
            .create_async()
            .await;

        // Empty errors array → should be treated as success
        let result = handle_graphql_query(&server.url(), "{ transactions { id } }", None).await;
        mock.assert_async().await;
        assert!(result.is_ok(), "empty errors should not fail: {result:?}");
    }

    #[tokio::test]
    async fn test_graphql_query_passes_variables() {
        let mut server = mockito::Server::new_async().await;
        // We just verify the POST reaches the server – body matching is
        // handled by integration tests.
        let mock = server
            .mock("POST", "/graphql")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data":{"transactions":[]}}"#)
            .create_async()
            .await;

        let result = handle_graphql_query(
            &server.url(),
            "{ transactions { id } }",
            Some(r#"{"filter":{"status":"pending"}}"#),
        )
        .await;
        mock.assert_async().await;
        assert!(result.is_ok());
    }

    // ─── handle_stats_status with mock server ────────────────────────────────

    #[tokio::test]
    async fn test_stats_status_table_output() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/stats/status")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"[
                    {"status":"completed","count":142},
                    {"status":"pending","count":12}
                ]"#,
            )
            .create_async()
            .await;

        let result = handle_stats_status(&server.url(), false).await;
        mock.assert_async().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_stats_status_json_output() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/stats/status")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"[{"status":"pending","count":5}]"#)
            .create_async()
            .await;

        let result = handle_stats_status(&server.url(), true).await;
        mock.assert_async().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_stats_status_server_error() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/stats/status")
            .with_status(500)
            .with_header("content-type", "application/json")
            .with_body(r#"{"error":"internal"}"#)
            .create_async()
            .await;

        let result = handle_stats_status(&server.url(), false).await;
        mock.assert_async().await;
        assert!(result.is_err(), "should fail on HTTP 500");
    }

    // ─── handle_stats_daily with mock server ─────────────────────────────────

    #[tokio::test]
    async fn test_stats_daily_sends_correct_days_param() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/stats/daily?days=14")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"[{"date":"2026-06-29","total_amount":"5000.00","tx_count":10}]"#)
            .create_async()
            .await;

        let result = handle_stats_daily(&server.url(), 14, false).await;
        mock.assert_async().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_stats_daily_json_output() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/stats/daily?days=7")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"[{"date":"2026-06-29","total_amount":"5000.00","tx_count":10}]"#)
            .create_async()
            .await;

        let result = handle_stats_daily(&server.url(), 7, true).await;
        mock.assert_async().await;
        assert!(result.is_ok());
    }

    // ─── handle_stats_assets with mock server ────────────────────────────────

    #[tokio::test]
    async fn test_stats_assets_table_output() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/stats/assets")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"[{"asset_code":"USDC","total_amount":"108402.25","tx_count":87,"avg_amount":"1245.43"}]"#,
            )
            .create_async()
            .await;

        let result = handle_stats_assets(&server.url(), false).await;
        mock.assert_async().await;
        assert!(result.is_ok());
    }

    // ─── handle_stats_cache with mock server ─────────────────────────────────

    #[tokio::test]
    async fn test_stats_cache_table_output() {
        let mut server = mockito::Server::new_async().await;
        let body = serde_json::json!({
            "query_cache": {
                "hits": 312, "misses": 48, "total": 360,
                "hit_rate": 0.8667,
                "memory_hits": 280, "memory_misses": 32,
                "memory_total": 312, "memory_hit_rate": 0.8974
            },
            "idempotency_cache_hits": 198,
            "idempotency_cache_misses": 22,
            "idempotency_lock_acquired": 210,
            "idempotency_lock_contention": 4,
            "idempotency_errors": 0,
            "idempotency_fallback_count": 2
        });
        let mock = server
            .mock("GET", "/cache/metrics")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body.to_string())
            .create_async()
            .await;

        let result = handle_stats_cache(&server.url(), false).await;
        mock.assert_async().await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_stats_cache_json_output() {
        let mut server = mockito::Server::new_async().await;
        let body = serde_json::json!({
            "query_cache": {"hits": 0, "misses": 0, "total": 0, "hit_rate": 0.0,
                "memory_hits": 0, "memory_misses": 0, "memory_total": 0, "memory_hit_rate": 0.0},
            "idempotency_cache_hits": 0,
            "idempotency_cache_misses": 0,
            "idempotency_lock_acquired": 0,
            "idempotency_lock_contention": 0,
            "idempotency_errors": 0,
            "idempotency_fallback_count": 0
        });
        let mock = server
            .mock("GET", "/cache/metrics")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body.to_string())
            .create_async()
            .await;

        let result = handle_stats_cache(&server.url(), true).await;
        mock.assert_async().await;
        assert!(result.is_ok());
    }
}

pub async fn handle_settlements_list(config: &Config, format: &str) -> anyhow::Result<()> {
    let base_url = format!("http://localhost:{}", config.server_port);
    let api_key = std::env::var("SYNAPSE_API_KEY").unwrap_or_else(|_| "dev-key".to_string());

    let client = synapse_sdk::SynapseClient::new(base_url, api_key);
    let params = synapse_sdk::SettlementParams::default();

    match client.settlements().list(params).await {
        Ok(response) => {
            match format {
                "json" => {
                    let json = serde_json::to_string_pretty(&response)?;
                    println!("{}", json);
                }
                _ => {
                    println!(
                        "{:<36} {:<12} {:<15} {:<10}",
                        "ID", "Status", "Total Amount", "Tx Count"
                    );
                    println!("{}", "-".repeat(73));
                    for settlement in &response.settlements {
                        println!(
                            "{:<36} {:<12} {:<15} {:<10}",
                            settlement.id,
                            settlement.status,
                            settlement.total_amount,
                            settlement.tx_count
                        );
                    }
                    if response.has_more {
                        println!(
                            "\n✓ {} settlements (more available)",
                            response.settlements.len()
                        );
                    } else {
                        println!("\n✓ {} settlements", response.settlements.len());
                    }
                }
            }
            Ok(())
        }
        Err(e) => {
            tracing::error!("Failed to list settlements: {}", e);
            anyhow::bail!("Failed to list settlements: {}", e)
        }
    }
}

pub async fn handle_settlements_get(config: &Config, id: &str, format: &str) -> anyhow::Result<()> {
    let base_url = format!("http://localhost:{}", config.server_port);
    let api_key = std::env::var("SYNAPSE_API_KEY").unwrap_or_else(|_| "dev-key".to_string());

    let client = synapse_sdk::SynapseClient::new(base_url, api_key);

    match client.settlements().get(id).await {
        Ok(settlement) => {
            match format {
                "json" => {
                    let json = serde_json::to_string_pretty(&settlement)?;
                    println!("{}", json);
                }
                _ => {
                    println!("ID:                    {}", settlement.id);
                    println!("Asset Code:            {}", settlement.asset_code);
                    println!("Total Amount:          {}", settlement.total_amount);
                    println!("Transaction Count:     {}", settlement.tx_count);
                    println!("Status:                {}", settlement.status);
                    println!("Period Start:          {}", settlement.period_start);
                    println!("Period End:            {}", settlement.period_end);
                    println!("Created At:            {}", settlement.created_at);
                    println!("Updated At:            {}", settlement.updated_at);
                    if let Some(reason) = settlement.dispute_reason {
                        println!("Dispute Reason:        {}", reason);
                    }
                    if let Some(amount) = settlement.original_total_amount {
                        println!("Original Total Amount: {}", amount);
                    }
                    if let Some(reviewer) = settlement.reviewed_by {
                        println!("Reviewed By:           {}", reviewer);
                    }
                }
            }
            Ok(())
        }
        Err(synapse_sdk::SynapseError::Http { status: 404, body }) => {
            tracing::warn!("Settlement {} not found: {}", id, body);
            anyhow::bail!("Settlement {} not found", id)
        }
        Err(e) => {
            tracing::error!("Failed to get settlement: {}", e);
            anyhow::bail!("Failed to get settlement: {}", e)
        }
    }
}

pub async fn handle_tx_search(
    config: &Config,
    status: Option<String>,
    asset_code: Option<String>,
    min_amount: Option<String>,
    max_amount: Option<String>,
    from: Option<String>,
    to: Option<String>,
    stellar_account: Option<String>,
    cursor: Option<String>,
    limit: i64,
    format: &str,
) -> anyhow::Result<()> {
    let base_url = format!("http://localhost:{}", config.server_port);
    let api_key = std::env::var("SYNAPSE_API_KEY").unwrap_or_else(|_| "dev-key".to_string());

    let client = synapse_sdk::SynapseClient::new(base_url, api_key);
    let params = synapse_sdk::SearchParams {
        status,
        asset_code,
        min_amount,
        max_amount,
        from,
        to,
        stellar_account,
        cursor,
        limit: Some(limit),
    };

    match client.transactions().search(params).await {
        Ok(response) => {
            match format {
                "json" => {
                    let json = serde_json::to_string_pretty(&response)?;
                    println!("{}", json);
                }
                _ => {
                    println!(
                        "{:<36} {:<12} {:<12} {:<15}",
                        "ID", "Status", "Asset", "Amount"
                    );
                    println!("{}", "-".repeat(75));
                    for tx in &response.results {
                        println!(
                            "{:<36} {:<12} {:<12} {:<15}",
                            tx.id, tx.status, tx.asset_code, tx.amount
                        );
                    }
                    println!(
                        "\n✓ {} results (total: {}",
                        response.results.len(),
                        response.total
                    );
                    if response.next_cursor.is_some() {
                        println!(
                            "  Use --cursor {} for next page",
                            response.next_cursor.as_ref().unwrap()
                        );
                    }
                    println!();
                }
            }
            Ok(())
        }
        Err(e) => {
            tracing::error!("Failed to search transactions: {}", e);
            anyhow::bail!("Failed to search transactions: {}", e)
        }
    }
}
