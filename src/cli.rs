use clap::{Parser, Subcommand};
use sqlx::PgPool;
use synapse_core::config::Config;
use uuid::Uuid;

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
}

#[derive(Subcommand)]
pub enum TxCommands {
    /// Force complete a transaction by ID
    ForceComplete {
        /// Transaction UUID
        #[arg(value_name = "TX_ID")]
        tx_id: Uuid,
    },

    #[command(long_about = "List transactions with cursor-based pagination and optional date filters.

All flags are optional. Cursors are opaque — always use next_cursor from previous response.
Invalid or expired cursors return an error and must not be retried as-is.

Examples:
  synapse-core tx list --limit 50
  synapse-core tx list --from-date 2024-01-01T00:00:00Z --to-date 2024-02-01T00:00:00Z
  synapse-core tx list --cursor <cursor> --format json")]
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

    #[command(long_about = "Search transactions by filter, returning a single page of matches.

All filters are optional — omit a field to leave that dimension unfiltered.
A search with no matches returns total=0 and empty results, not an error.

Examples:
  synapse-core tx search --status completed --asset-code USD
  synapse-core tx search --min-amount 10.00 --max-amount 500.00
  synapse-core tx search --stellar-account GBRPYHIL2CI3WHZDTOOQFC6EB4KJJGUJIIAY3XDBKWV3UYSI7IFYWU4")]
    Search {
        /// Exact transaction status (e.g., pending, completed)
        #[arg(long)]
        status: Option<String>,

        /// Exact asset code (e.g., USD)
        #[arg(long)]
        asset_code: Option<String>,

        /// Inclusive minimum amount as decimal (e.g., 10.00)
        #[arg(long)]
        min_amount: Option<String>,

        /// Inclusive maximum amount as decimal (e.g., 500.00)
        #[arg(long)]
        max_amount: Option<String>,

        /// Inclusive RFC 3339 range start (e.g., 2024-01-01T00:00:00Z)
        #[arg(long)]
        from: Option<String>,

        /// Exclusive RFC 3339 range end (e.g., 2024-02-01T00:00:00Z)
        #[arg(long)]
        to: Option<String>,

        /// Exact Stellar account to filter by
        #[arg(long)]
        stellar_account: Option<String>,

        /// Opaque pagination cursor (use next_cursor from previous response)
        #[arg(long)]
        cursor: Option<String>,

        /// Maximum records per page (server default: 25, max: 100)
        #[arg(long, short = 'l')]
        limit: Option<i64>,

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
    use chrono::DateTime;
    use synapse_core::services::ReconciliationService;
    use synapse_core::stellar::HorizonClient;

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

pub async fn handle_settlements_list(config: &Config, format: &str) -> anyhow::Result<()> {
    let base_url = format!("http://localhost:{}", config.server_port);
    let api_key = std::env::var("SYNAPSE_API_KEY").unwrap_or_else(|_| "dev-key".to_string());

    let client = synapse_sdk::SynapseClient::new(base_url, api_key);
    let params = synapse_sdk::ListParams::default();

    match client.settlements().list(params).await {
        Ok(response) => {
            match format {
                "json" => {
                    let json = serde_json::to_string_pretty(&response)?;
                    println!("{}", json);
                }
                _ => {
                    println!("{:<36} {:<12} {:<15} {:<10}", "ID", "Status", "Total Amount", "Tx Count");
                    println!("{}", "-".repeat(73));
                    for settlement in &response.settlements {
                        println!(
                            "{:<36} {:<12} {:<15} {:<10}",
                            settlement.id, settlement.status, settlement.total_amount, settlement.tx_count
                        );
                    }
                    if response.has_more {
                        println!("\n✓ {} settlements (more available)", response.settlements.len());
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
                    println!("{:<36} {:<12} {:<12} {:<15}", "ID", "Status", "Asset", "Amount");
                    println!("{}", "-".repeat(75));
                    for tx in &response.results {
                        println!(
                            "{:<36} {:<12} {:<12} {:<15}",
                            tx.id, tx.status, tx.asset_code, tx.amount
                        );
                    }
                    println!("\n✓ {} results (total: {}", response.results.len(), response.total);
                    if response.next_cursor.is_some() {
                        println!("  Use --cursor {} for next page", response.next_cursor.as_ref().unwrap());
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
