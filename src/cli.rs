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

pub async fn handle_backup_run(config: &Config, backup_type_str: &str) -> anyhow::Result<()> {
    use crate::services::backup::{BackupService, BackupType};
    use std::path::PathBuf;

    let backup_type = match backup_type_str.to_lowercase().as_str() {
        "hourly" => BackupType::Hourly,
        "daily" => BackupType::Daily,
        "monthly" => BackupType::Monthly,
        _ => anyhow::bail!("Invalid backup type. Use: hourly, daily, or monthly"),
    };

    let service = BackupService::new(
        config.database_url.clone(),
        PathBuf::from(&config.backup_dir),
        config.backup_encryption_key.clone(),
    );

    tracing::info!("Creating {:?} backup...", backup_type);
    println!("Creating {:?} backup...", backup_type);

    let metadata = service.create_backup(backup_type).await?;

    println!("✓ Backup created successfully:");
    println!("  Filename: {}", metadata.filename);
    println!("  Size: {} bytes", metadata.size_bytes);
    println!("  Compressed: {}", metadata.compressed);
    println!("  Encrypted: {}", metadata.encrypted);

    Ok(())
}

pub async fn handle_backup_list(config: &Config) -> anyhow::Result<()> {
    use crate::services::backup::BackupService;
    use std::path::PathBuf;

    let service = BackupService::new(
        config.database_url.clone(),
        PathBuf::from(&config.backup_dir),
        config.backup_encryption_key.clone(),
    );

    let backups = service.list_backups().await?;

    if backups.is_empty() {
        println!("No backups found");
        return Ok(());
    }

    println!("Available backups:");
    println!("{:<40} {:<10} {:<20} {:<12}", "Filename", "Type", "Timestamp", "Size");
    println!("{}", "-".repeat(85));

    for backup in backups {
        println!(
            "{:<40} {:<10} {:<20} {:<12}",
            backup.filename,
            format!("{:?}", backup.backup_type),
            backup.timestamp.format("%Y-%m-%d %H:%M:%S"),
            format_size(backup.size_bytes)
        );
    }

    Ok(())
}

pub async fn handle_backup_restore(config: &Config, filename: &str) -> anyhow::Result<()> {
    use crate::services::backup::BackupService;
    use std::path::PathBuf;

    let service = BackupService::new(
        config.database_url.clone(),
        PathBuf::from(&config.backup_dir),
        config.backup_encryption_key.clone(),
    );

    println!("⚠️  WARNING: This will replace the current database!");
    println!("Restoring from: {}", filename);
    println!("Press Ctrl+C to cancel, or wait 5 seconds to continue...");

    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

    tracing::info!("Restoring backup: {}", filename);
    service.restore_backup(filename).await?;

    println!("✓ Backup restored successfully");

    Ok(())
}

pub async fn handle_backup_cleanup(config: &Config) -> anyhow::Result<()> {
    use crate::services::backup::BackupService;
    use std::path::PathBuf;

    let service = BackupService::new(
        config.database_url.clone(),
        PathBuf::from(&config.backup_dir),
        config.backup_encryption_key.clone(),
    );

    tracing::info!("Applying retention policy...");
    println!("Applying retention policy...");

    service.apply_retention_policy().await?;

    println!("✓ Retention policy applied successfully");

    Ok(())
}

fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}
