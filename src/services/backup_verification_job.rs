use crate::services::{backup::BackupService, scheduler::Job};
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Instant;

/// Weekly job that verifies the latest backup's on-disk integrity via
/// checksum comparison (`BackupService::verify_backup_checksum`).
///
/// # Design decision: checksum-only, not restore-and-verify
///
/// This deliberately does not restore the backup anywhere. `BackupService`'s
/// only restore path (`restore_backup`) always targets `self.database_url`,
/// which in every deployment of this service is the same database the live
/// application depends on — there is no isolated/scratch-database target
/// available to restore into. A scheduled job that called `restore_backup`
/// unattended would run a full logical restore against production every
/// week. Checksum verification proves the backup *file* is intact without
/// touching any database, which is the safe, correct scope for a routine,
/// automated, repeated check. See `docs/backup_verification.md` for the
/// full tradeoff and what a future full restore-to-scratch-target path
/// would require.
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
        let started = Instant::now();

        let result = match self.backup_service.list_backups().await {
            Ok(backups) => match backups.first() {
                Some(latest) => {
                    tracing::info!("Verifying latest backup: {}", latest.filename);
                    match self
                        .backup_service
                        .verify_backup_checksum(&latest.filename)
                        .await
                    {
                        Ok(metadata) => {
                            tracing::info!(
                                "Backup verification succeeded: filename={}, checksum={}, \
                                 size_bytes={}",
                                metadata.filename,
                                metadata.checksum,
                                metadata.size_bytes
                            );
                            "success"
                        }
                        Err(e) => {
                            if let Some(mismatch) =
                                e.downcast_ref::<crate::services::backup::ChecksumMismatch>()
                            {
                                tracing::error!(
                                    filename = %mismatch.filename,
                                    expected_checksum = %mismatch.expected_checksum,
                                    actual_checksum = %mismatch.actual_checksum,
                                    "Backup verification FAILED: checksum mismatch"
                                );
                            } else {
                                tracing::error!(
                                    "Backup verification FAILED for {}: {e}",
                                    latest.filename
                                );
                            }
                            "failure"
                        }
                    }
                }
                None => {
                    tracing::warn!("No backups found for verification");
                    "no_backups"
                }
            },
            Err(e) => {
                tracing::error!("Failed to list backups: {}", e);
                "failure"
            }
        };

        crate::metrics::backup_verification_total()
            .add(1, &[opentelemetry::KeyValue::new("result", result)]);
        crate::metrics::backup_verification_duration_ms()
            .record(started.elapsed().as_secs_f64() * 1000.0, &[]);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::backup::BackupType;
    use std::path::PathBuf;

    async fn service_with_temp_dir() -> (Arc<BackupService>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let service = Arc::new(BackupService::new(
            // No real database needed: BackupService::new doesn't connect;
            // create_backup below is the only call that would need pg_dump
            // on PATH, which CI's runner has via the postgres-client tooling
            // this workflow already installs for sqlx-cli/migrations.
            "postgres://synapse:synapse@localhost:5432/synapse_test".to_string(),
            PathBuf::from(dir.path()),
            None,
        ));
        (service, dir)
    }

    /// Task item 4 (first test): corrupting a backup artifact must be
    /// detected by the scheduled job's verification path.
    #[ignore = "Requires pg_dump on PATH and a reachable Postgres"]
    #[tokio::test]
    async fn corrupted_backup_is_detected() {
        let (service, dir) = service_with_temp_dir().await;
        let metadata = service.create_backup(BackupType::Hourly).await.unwrap();

        // Corrupt the backup file's contents in place.
        let backup_path = dir.path().join(&metadata.filename);
        tokio::fs::write(&backup_path, b"not a valid backup anymore")
            .await
            .unwrap();

        let job = BackupVerificationJob::new(service.clone());
        // execute() itself always returns Ok (the job never propagates a
        // corruption as a scheduler-level failure — see backup_verification_total
        // for the outcome signal); assert on the underlying verification
        // call directly so this test fails loudly if detection breaks.
        let result = service.verify_backup_checksum(&metadata.filename).await;
        let err = result.expect_err("corrupted backup must fail checksum verification");
        let mismatch = err
            .downcast_ref::<crate::services::backup::ChecksumMismatch>()
            .expect("failure must be a structured ChecksumMismatch report");
        assert_eq!(mismatch.filename, metadata.filename);
        assert_eq!(mismatch.expected_checksum, metadata.checksum);
        assert_ne!(mismatch.actual_checksum, metadata.checksum);
        assert!(!mismatch.actual_checksum.is_empty());
        job.execute().await.unwrap();
    }

    /// Corrupted fixture (no pg_dump/Postgres needed): the structured
    /// mismatch report must name the file and both checksums.
    #[tokio::test]
    async fn corrupted_fixture_reports_file_and_both_checksums() {
        let (service, dir) = service_with_temp_dir().await;
        let filename = "backup_hourly_20260101_000000.sql.gz";
        let backup_path = dir.path().join(filename);
        tokio::fs::write(&backup_path, b"corrupted fixture contents")
            .await
            .unwrap();

        let metadata = crate::services::backup::BackupMetadata {
            filename: filename.to_string(),
            backup_type: BackupType::Hourly,
            timestamp: chrono::Utc::now(),
            size_bytes: 26,
            compressed: true,
            encrypted: false,
            checksum: "0000000000000000000000000000000000000000000000000000000000000000"
                .to_string(),
        };
        tokio::fs::write(
            dir.path().join(format!("{filename}.meta")),
            serde_json::to_string(&metadata).unwrap(),
        )
        .await
        .unwrap();

        let err = service
            .verify_backup_checksum(filename)
            .await
            .expect_err("corrupted fixture must fail verification");
        let mismatch = err
            .downcast_ref::<crate::services::backup::ChecksumMismatch>()
            .expect("failure must be a structured ChecksumMismatch report");

        assert_eq!(mismatch.filename, filename);
        assert_eq!(mismatch.expected_checksum, metadata.checksum);
        assert_ne!(mismatch.actual_checksum, metadata.checksum);
        assert!(!mismatch.actual_checksum.is_empty());
        assert!(mismatch.to_string().contains(filename));
        assert!(mismatch.to_string().contains(&metadata.checksum));
        assert!(mismatch.to_string().contains(&mismatch.actual_checksum));
    }

    /// Task item 4 (second test): verification must never modify the
    /// backup file or touch any database — checksum-only by construction,
    /// but assert the file is byte-for-byte unchanged as a concrete check.
    #[ignore = "Requires pg_dump on PATH and a reachable Postgres"]
    #[tokio::test]
    async fn verification_does_not_modify_backup_file() {
        let (service, dir) = service_with_temp_dir().await;
        let metadata = service.create_backup(BackupType::Hourly).await.unwrap();
        let backup_path = dir.path().join(&metadata.filename);
        let before = tokio::fs::read(&backup_path).await.unwrap();

        let result = service.verify_backup_checksum(&metadata.filename).await;
        assert!(result.is_ok(), "valid backup should verify successfully");

        let after = tokio::fs::read(&backup_path).await.unwrap();
        assert_eq!(
            before, after,
            "verification must not modify the backup file"
        );
    }
}
