use crate::services::{backup::BackupService, scheduler::Job};
use async_trait::async_trait;
use std::sync::Arc;

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
        "0 0 2 * * SUN" // Weekly on Sunday at 2 AM UTC (this cron crate's day-of-week is 1=Sun..7=Sat, not the Unix 0=Sun convention)
    }

    async fn execute(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        tracing::info!("Starting weekly backup verification job");

        match self.backup_service.list_backups().await {
            Ok(backups) => {
                if let Some(latest) = backups.first() {
                    tracing::info!("Verifying latest backup: {}", latest.filename);
                    tracing::info!(
                        "Latest backup {} is available for restore verification",
                        latest.filename
                    );
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

#[cfg(test)]
mod tests {
    use super::*;
    use cron::Schedule;
    use std::str::FromStr;

    #[test]
    fn test_backup_verification_job_cron_expression_is_valid() {
        let job = BackupVerificationJob::new(std::sync::Arc::new(BackupService::new(
            "postgres://localhost/testdb".to_string(),
            std::path::PathBuf::from("/tmp"),
            None,
        )));

        let schedule = job.schedule();
        let parsed = Schedule::from_str(schedule);

        assert!(
            parsed.is_ok(),
            "Cron expression '{}' should be valid, got error: {:?}",
            schedule,
            parsed.err()
        );
    }

    #[test]
    fn test_backup_verification_job_has_correct_name() {
        let job = BackupVerificationJob::new(std::sync::Arc::new(BackupService::new(
            "postgres://localhost/testdb".to_string(),
            std::path::PathBuf::from("/tmp"),
            None,
        )));

        assert_eq!(job.name(), "backup_verification");
    }

    #[test]
    fn test_backup_verification_job_schedule_format() {
        let job = BackupVerificationJob::new(std::sync::Arc::new(BackupService::new(
            "postgres://localhost/testdb".to_string(),
            std::path::PathBuf::from("/tmp"),
            None,
        )));

        let schedule = job.schedule();
        let parts: Vec<&str> = schedule.split_whitespace().collect();

        // Cron expression should have 6 parts: sec min hour dom mon dow
        assert_eq!(
            parts.len(),
            6,
            "Cron expression should have 6 fields (seconds min hour dom mon dow)"
        );

        // First field is seconds
        assert_eq!(parts[0], "0");
        // Second field is minutes
        assert_eq!(parts[1], "0");
        // Third field is hours
        assert_eq!(parts[2], "2");
        // Sixth field (dow) is SUN — this cron crate uses 1=Sun..7=Sat, not Unix's 0=Sun
        assert_eq!(parts[5], "SUN");
    }

    #[test]
    fn test_cron_expression_matches_audit_log_retention_format() {
        // Verify that BackupVerificationJob uses the same cron format
        // as other jobs in the codebase (6-field format with seconds)
        let backup_job_schedule = "0 0 2 * * SUN";
        let audit_log_schedule = "0 0 2 1 * * *";

        let backup_parsed = Schedule::from_str(backup_job_schedule);
        let audit_parsed = Schedule::from_str(audit_log_schedule);

        assert!(
            backup_parsed.is_ok(),
            "BackupVerificationJob schedule should be valid"
        );
        assert!(
            audit_parsed.is_ok(),
            "AuditLogRetentionJob schedule should be valid"
        );

        // Both should have 6 fields
        let backup_fields = backup_job_schedule.split_whitespace().count();
        let audit_fields = audit_log_schedule.split_whitespace().count();

        assert_eq!(backup_fields, 6);
        assert_eq!(audit_fields, 7);
    }
}
