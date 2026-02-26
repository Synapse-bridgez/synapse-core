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
            println!("✓ Transaction {} marked as completed", tx_id);
            Ok(())
        }
        None => {
            tracing::warn!("Transaction {} not found", tx_id);
            anyhow::bail!("Transaction {} not found", tx_id)
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
                return format!("{}{}:****{}", prefix, user, suffix);
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
            println!("{}", json);
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
