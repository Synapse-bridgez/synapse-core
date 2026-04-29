use async_trait::async_trait;
use std::sync::Arc;
use crate::services::{backup::BackupService, scheduler::Job};

pub struct BackupVerificationJob {
    backup_service: Arc<BackupService>,
}

impl BackupVerificationJob {
    pub fn new(backup_service: Arc<BackupService>) -> Self {
        Self { backup_service }
    }
}

#[async_trait]
impl Job for BackupVerificationJob {
    fn name(&self) -> &str {
        "backup_verification"
    }

    fn schedule(&self) -> &str {
        "0 2 * * 0" // Weekly on Sunday at 2 AM
    }

    async fn execute(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        tracing::info!("Starting weekly backup verification job");

        match self.backup_service.list_backups().await {
            Ok(backups) => {
                if let Some(latest) = backups.first() {
                    tracing::info!("Verifying latest backup: {}", latest.filename);
                    match self.backup_service.verify_backup_integrity(&latest.filename).await {
                        Ok(result) => {
                            tracing::info!(
                                "Backup verification completed: status={}, rows={:?}",
                                result.verification_status,
                                result.row_count
                            );
                        }
                        Err(e) => {
                            tracing::error!("Backup verification failed: {}", e);
                        }
                    }
                } else {
                    tracing::warn!("No backups found for verification");
                }
            }
            Err(e) => {
                tracing::error!("Failed to list backups: {}", e);
            }
        }

        Ok(())
    }
}
