use clap::{Parser, Subcommand};
use sqlx::PgPool;
use uuid::Uuid;
use crate::config::Config;

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
}

#[derive(Subcommand)]
pub enum DbCommands {
    /// Run database migrations
    Migrate,
}

pub async fn handle_tx_force_complete(pool: &PgPool, tx_id: Uuid) -> anyhow::Result<()> {
    let result = sqlx::query(
        "UPDATE transactions SET status = 'completed', updated_at = NOW() WHERE id = $1 RETURNING id"
    )
    .bind(tx_id)
    .fetch_optional(pool)
    .await?;

    match result {
        Some(_) => {
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
