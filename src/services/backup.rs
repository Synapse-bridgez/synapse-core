use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::path::{Path, PathBuf};
use std::process::Command;
use tokio::fs;
use tokio::io::AsyncWriteExt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackupType {
    Hourly,
    Daily,
    Monthly,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupMetadata {
    pub filename: String,
    pub backup_type: BackupType,
    pub timestamp: DateTime<Utc>,
    pub size_bytes: u64,
    pub compressed: bool,
    pub encrypted: bool,
    pub checksum: String,
}

pub struct BackupService {
    database_url: String,
    backup_dir: PathBuf,
    encryption_key: Option<String>,
}

impl BackupService {
    pub fn new(database_url: String, backup_dir: PathBuf, encryption_key: Option<String>) -> Self {
        Self {
            database_url,
            backup_dir,
            encryption_key,
        }
    }

    pub async fn create_backup(&self, backup_type: BackupType) -> Result<BackupMetadata> {
        // Ensure backup directory exists
        fs::create_dir_all(&self.backup_dir)
            .await
            .context("Failed to create backup directory")?;

        let timestamp = Utc::now();
        let filename = self.generate_filename(backup_type, timestamp);
        let backup_path = self.backup_dir.join(&filename);
        let temp_path = self.backup_dir.join(format!("{filename}.tmp"));

        // Run pg_dump
        tracing::info!("Running pg_dump for {:?} backup", backup_type);
        self.run_pg_dump(&temp_path).await?;

        // Compress the backup
        tracing::info!("Compressing backup");
        let compressed_path = self.compress_backup(&temp_path).await?;

        // Encrypt if key is provided
        let final_path = if self.encryption_key.is_some() {
            tracing::info!("Encrypting backup");
            self.encrypt_backup(&compressed_path).await?
        } else {
            compressed_path
        };

        // Move to final location
        fs::rename(&final_path, &backup_path)
            .await
            .context("Failed to move backup to final location")?;

        // Calculate checksum
        let checksum = self.calculate_checksum(&backup_path).await?;

        // Get file size
        let metadata = fs::metadata(&backup_path)
            .await
            .context("Failed to get backup file metadata")?;

        let backup_metadata = BackupMetadata {
            filename,
            backup_type,
            timestamp,
            size_bytes: metadata.len(),
            compressed: true,
            encrypted: self.encryption_key.is_some(),
            checksum,
        };

        // Save metadata
        self.save_metadata(&backup_metadata).await?;

        tracing::info!("Backup created successfully: {}", backup_metadata.filename);

        Ok(backup_metadata)
    }

    pub async fn list_backups(&self) -> Result<Vec<BackupMetadata>> {
        let mut backups = Vec::new();

        if !self.backup_dir.exists() {
            return Ok(backups);
        }

        let mut entries = fs::read_dir(&self.backup_dir)
            .await
            .context("Failed to read backup directory")?;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("meta") {
                if let Ok(metadata) = self.load_metadata(&path).await {
                    backups.push(metadata);
                }
            }
        }

        // Sort by timestamp descending
        backups.sort_by_key(|b| Reverse(b.timestamp));

        Ok(backups)
    }

    pub async fn restore_backup(&self, filename: &str) -> Result<()> {
        let backup_path = self.backup_dir.join(filename);

        if !backup_path.exists() {
            anyhow::bail!("Backup file not found: {filename}");
        }

        // Load and verify metadata
        let meta_path = backup_path.with_extension("meta");
        let metadata = self.load_metadata(&meta_path).await?;

        tracing::info!("Verifying backup integrity");
        self.verify_backup(&backup_path, &metadata).await?;

        let temp_dir = self.backup_dir.join("restore_temp");
        fs::create_dir_all(&temp_dir)
            .await
            .context("Failed to create temp directory")?;

        let mut current_path = backup_path.clone();

        // Decrypt if encrypted
        if metadata.encrypted {
            tracing::info!("Decrypting backup");
            current_path = self.decrypt_backup(&current_path, &temp_dir).await?;
        }

        // Decompress
        tracing::info!("Decompressing backup");
        let sql_path = self.decompress_backup(&current_path, &temp_dir).await?;

        // Restore to database
        tracing::info!("Restoring to database");
        self.run_pg_restore(&sql_path).await?;

        // Cleanup temp directory
        fs::remove_dir_all(&temp_dir)
            .await
            .context("Failed to cleanup temp directory")?;

        tracing::info!("Backup restored successfully");

        Ok(())
    }

    pub async fn apply_retention_policy(&self) -> Result<()> {
        let backups = self.list_backups().await?;

        let mut hourly_backups = Vec::new();
        let mut daily_backups = Vec::new();
        let mut monthly_backups = Vec::new();

        for backup in backups {
            match backup.backup_type {
                BackupType::Hourly => hourly_backups.push(backup),
                BackupType::Daily => daily_backups.push(backup),
                BackupType::Monthly => monthly_backups.push(backup),
            }
        }

        // Keep last 24 hourly
        self.apply_retention(&hourly_backups, 24).await?;

        // Keep last 30 daily
        self.apply_retention(&daily_backups, 30).await?;

        // Keep last 12 monthly
        self.apply_retention(&monthly_backups, 12).await?;

        Ok(())
    }

    async fn apply_retention(&self, backups: &[BackupMetadata], keep_count: usize) -> Result<()> {
        if backups.len() <= keep_count {
            return Ok(());
        }

        for backup in &backups[keep_count..] {
            let backup_path = self.backup_dir.join(&backup.filename);
            let meta_path = backup_path.with_extension("meta");

            tracing::info!("Removing old backup: {}", backup.filename);

            if backup_path.exists() {
                fs::remove_file(&backup_path)
                    .await
                    .context("Failed to remove backup file")?;
            }

            if meta_path.exists() {
                fs::remove_file(&meta_path)
                    .await
                    .context("Failed to remove metadata file")?;
            }
        }

        Ok(())
    }

    async fn run_pg_dump(&self, output_path: &Path) -> Result<()> {
        let url = std::env::var("DATABASE_URL")
            .or_else(|_| std::env::var("PITR_DATABASE_URL"))
            .unwrap_or_else(|_| self.database_url.clone());

        let mut cmd = Command::new("pg_dump");

        if let Ok(url_parsed) = url::Url::parse(&url) {
            if let Some(password) = url_parsed.password() {
                cmd.env("PGPASSWORD", password);
            }
            let db_url_no_creds = format!(
                "{}://{}@{}:{}/{}",
                url_parsed.scheme(),
                url_parsed.username(),
                url_parsed.host_str().unwrap_or("localhost"),
                url_parsed.port().unwrap_or(5432),
                url_parsed.path().trim_start_matches('/')
            );
            cmd.arg(&db_url_no_creds);
        } else {
            cmd.arg(&url);
        }

        let output = cmd
            .arg("--format=plain")
            .arg("--no-owner")
            .arg("--no-acl")
            .arg(format!("--file={}", output_path.display()))
            .output()
            .context("Failed to execute pg_dump")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("pg_dump failed: {stderr}");
        }

        Ok(())
    }

    async fn run_pg_restore(&self, sql_path: &Path) -> Result<()> {
        let url = std::env::var("DATABASE_URL")
            .or_else(|_| std::env::var("PITR_DATABASE_URL"))
            .unwrap_or_else(|_| self.database_url.clone());

        let mut cmd = Command::new("psql");

        if let Ok(url_parsed) = url::Url::parse(&url) {
            if let Some(password) = url_parsed.password() {
                cmd.env("PGPASSWORD", password);
            }
            let db_url_no_creds = format!(
                "{}://{}@{}:{}/{}",
                url_parsed.scheme(),
                url_parsed.username(),
                url_parsed.host_str().unwrap_or("localhost"),
                url_parsed.port().unwrap_or(5432),
                url_parsed.path().trim_start_matches('/')
            );
            cmd.arg(&db_url_no_creds);
        } else {
            cmd.arg(&url);
        }

        let output = cmd
            .arg("--file")
            .arg(sql_path)
            .output()
            .context("Failed to execute psql")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("psql restore failed: {stderr}");
        }

        Ok(())
    }

    async fn compress_backup(&self, input_path: &Path) -> Result<PathBuf> {
        let output_path = input_path.with_extension("sql.gz");

        let output = Command::new("gzip")
            .arg("-c")
            .arg(input_path)
            .output()
            .context("Failed to execute gzip")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("gzip failed: {stderr}");
        }

        let mut file = fs::File::create(&output_path)
            .await
            .context("Failed to create compressed file")?;

        file.write_all(&output.stdout)
            .await
            .context("Failed to write compressed data")?;

        // Remove temp file
        fs::remove_file(input_path)
            .await
            .context("Failed to remove temp file")?;

        Ok(output_path)
    }

    async fn decompress_backup(&self, input_path: &Path, temp_dir: &Path) -> Result<PathBuf> {
        let output_path = temp_dir.join("restore.sql");

        let output = Command::new("gunzip")
            .arg("-c")
            .arg(input_path)
            .output()
            .context("Failed to execute gunzip")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("gunzip failed: {stderr}");
        }

        let mut file = fs::File::create(&output_path)
            .await
            .context("Failed to create decompressed file")?;

        file.write_all(&output.stdout)
            .await
            .context("Failed to write decompressed data")?;

        Ok(output_path)
    }

    async fn encrypt_backup(&self, input_path: &Path) -> Result<PathBuf> {
        let key = self
            .encryption_key
            .as_ref()
            .context("Encryption key not provided")?;

        let output_path = input_path.with_extension("sql.gz.enc");

        let mut cmd = Command::new("openssl");
        cmd.arg("enc")
            .arg("-aes-256-cbc")
            .arg("-salt")
            .arg("-pbkdf2")
            .arg("-in")
            .arg(input_path)
            .arg("-out")
            .arg(&output_path)
            .arg("-pass")
            .arg("env:OPENSSL_PASS");

        cmd.env("OPENSSL_PASS", key);

        let output = cmd.output().context("Failed to execute openssl")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("openssl encryption failed: {stderr}");
        }

        // Remove unencrypted file
        fs::remove_file(input_path)
            .await
            .context("Failed to remove unencrypted file")?;

        Ok(output_path)
    }

    async fn decrypt_backup(&self, input_path: &Path, temp_dir: &Path) -> Result<PathBuf> {
        let key = self
            .encryption_key
            .as_ref()
            .context("Encryption key not provided")?;

        let output_path = temp_dir.join("decrypted.sql.gz");

        let mut cmd = Command::new("openssl");
        cmd.arg("enc")
            .arg("-aes-256-cbc")
            .arg("-d")
            .arg("-pbkdf2")
            .arg("-in")
            .arg(input_path)
            .arg("-out")
            .arg(&output_path)
            .arg("-pass")
            .arg("env:OPENSSL_PASS");

        cmd.env("OPENSSL_PASS", key);

        let output = cmd.output().context("Failed to execute openssl")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("openssl decryption failed: {stderr}");
        }

        Ok(output_path)
    }

    async fn calculate_checksum(&self, path: &Path) -> Result<String> {
        let output = Command::new("sha256sum")
            .arg(path)
            .output()
            .context("Failed to execute sha256sum")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("sha256sum failed: {stderr}");
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let checksum = stdout
            .split_whitespace()
            .next()
            .context("Failed to parse checksum")?
            .to_string();

        Ok(checksum)
    }

    async fn verify_backup(&self, path: &Path, metadata: &BackupMetadata) -> Result<()> {
        let checksum = self.calculate_checksum(path).await?;

        if checksum != metadata.checksum {
            anyhow::bail!(
                "Backup integrity check failed: checksum mismatch (expected: {}, got: {})",
                metadata.checksum,
                checksum
            );
        }

        Ok(())
    }

    fn generate_filename(&self, backup_type: BackupType, timestamp: DateTime<Utc>) -> String {
        let type_str = match backup_type {
            BackupType::Hourly => "hourly",
            BackupType::Daily => "daily",
            BackupType::Monthly => "monthly",
        };

        let date_str = timestamp.format("%Y%m%d_%H%M%S");
        let extension = if self.encryption_key.is_some() {
            "sql.gz.enc"
        } else {
            "sql.gz"
        };

        format!("backup_{type_str}_{date_str}.{extension}")
    }

    async fn save_metadata(&self, metadata: &BackupMetadata) -> Result<()> {
        let meta_path = self
            .backup_dir
            .join(&metadata.filename)
            .with_extension("meta");

        let json =
            serde_json::to_string_pretty(metadata).context("Failed to serialize metadata")?;

        fs::write(&meta_path, json)
            .await
            .context("Failed to write metadata file")?;

        Ok(())
    }

    async fn load_metadata(&self, path: &Path) -> Result<BackupMetadata> {
        let json = fs::read_to_string(path)
            .await
            .context("Failed to read metadata file")?;

        let metadata: BackupMetadata =
            serde_json::from_str(&json).context("Failed to parse metadata")?;

        Ok(metadata)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backup_service_creation() {
        let service = BackupService::new(
            "postgres://user:password@localhost/testdb".to_string(),
            PathBuf::from("/tmp/backups"),
            Some("encryption_key".to_string()),
        );

        assert_eq!(service.database_url, "postgres://user:password@localhost/testdb");
        assert_eq!(service.encryption_key, Some("encryption_key".to_string()));
    }

    #[test]
    fn test_backup_metadata_creation() {
        let now = Utc::now();
        let metadata = BackupMetadata {
            filename: "backup_2024_01_01.sql.gz.enc".to_string(),
            backup_type: BackupType::Daily,
            timestamp: now,
            size_bytes: 1024,
            compressed: true,
            encrypted: true,
            checksum: "abc123".to_string(),
        };

        assert_eq!(metadata.backup_type, BackupType::Daily);
        assert!(metadata.encrypted);
        assert!(metadata.compressed);
    }

    #[test]
    fn test_backup_type_variants() {
        assert_eq!(BackupType::Hourly, BackupType::Hourly);
        assert_eq!(BackupType::Daily, BackupType::Daily);
        assert_eq!(BackupType::Monthly, BackupType::Monthly);
        assert_ne!(BackupType::Hourly, BackupType::Daily);
    }

    #[test]
    fn test_database_url_parsing() {
        let url_str = "postgres://user:password@localhost:5432/mydb";
        let parsed = url::Url::parse(url_str).expect("Failed to parse URL");

        assert_eq!(parsed.scheme(), "postgres");
        assert_eq!(parsed.username(), "user");
        assert_eq!(parsed.password(), Some("password"));
        assert_eq!(parsed.host_str(), Some("localhost"));
        assert_eq!(parsed.port(), Some(5432));

        let path = parsed.path().trim_start_matches('/');
        assert_eq!(path, "mydb");
    }

    #[test]
    fn test_database_url_without_password() {
        let url_str = "postgres://localhost/mydb";
        let parsed = url::Url::parse(url_str).expect("Failed to parse URL");

        assert_eq!(parsed.scheme(), "postgres");
        assert_eq!(parsed.password(), None);
    }

    #[test]
    fn test_backup_metadata_serialization() {
        let now = Utc::now();
        let metadata = BackupMetadata {
            filename: "test_backup.sql.gz".to_string(),
            backup_type: BackupType::Hourly,
            timestamp: now,
            size_bytes: 5000,
            compressed: true,
            encrypted: false,
            checksum: "def456".to_string(),
        };

        let json = serde_json::to_string(&metadata).expect("Serialization failed");
        let deserialized: BackupMetadata =
            serde_json::from_str(&json).expect("Deserialization failed");

        assert_eq!(deserialized.filename, metadata.filename);
        assert_eq!(deserialized.backup_type, metadata.backup_type);
        assert_eq!(deserialized.size_bytes, metadata.size_bytes);
    }
}
