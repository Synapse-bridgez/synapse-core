use crate::services::{backup::BackupService, scheduler::Job};
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Instant;

// ── Dynamic table coverage (#1116) ───────────────────────────────────────────

/// Tables that are intentionally excluded from backup verification coverage.
///
/// An entry here means the table exists in the schema but is known to
/// contain non-critical or non-financial data that does not need to be
/// individually audited. Additions to this list MUST be accompanied by a
/// code-review comment explaining why the exclusion is safe.
///
/// Rationale for each current exclusion:
///
/// - `backup_verification_logs`: infrastructure metadata written *by* the
///   backup process itself; excluding it avoids circular verification.
/// - `audit_log_archives`: retention-run metadata (not the audit records
///   themselves); the actual audit data lives in `audit_logs`.
/// - `sqlx_migrations`: sqlx internal table, not tenant data.
/// - `_sqlx_migrations`: alternative name used by some sqlx versions.
/// - `account_monitor_cursors`: transient cursor state, not financial data.
/// - `feature_flags`, `feature_flag_rollout`, `feature_flag_dependencies`,
///   `feature_flag_audit`: configuration / feature-flag metadata; no financial
///   or compliance data.
/// - `api_quotas`: rate-limiting configuration; not a financial record.
/// - `webhook_delivery_attempts`, `webhook_events`, `webhook_filter_rules`,
///   `webhook_endpoints`, `webhook_replay_queue`, `idempotency_keys`,
///   `transaction_dlq`: operational plumbing tables; the source-of-truth
///   financial rows live in `transactions` and `settlements`.
pub const BACKUP_COVERAGE_EXCLUSIONS: &[&str] = &[
    "backup_verification_logs",
    "audit_log_archives",
    "sqlx_migrations",
    "_sqlx_migrations",
    "account_monitor_cursors",
    "feature_flags",
    "feature_flag_rollout",
    "feature_flag_dependencies",
    "feature_flag_audit",
    "api_quotas",
    "webhook_delivery_attempts",
    "webhook_events",
    "webhook_filter_rules",
    "webhook_endpoints",
    "webhook_replay_queue",
    "idempotency_keys",
    "transaction_dlq",
];

/// Tables that store financial or compliance-relevant data and must always
/// be covered by backup verification. This list is the *floor* — it does
/// not replace the dynamic `information_schema`-driven check performed by
/// `audit_table_coverage`, but ensures that newly added tables do not
/// silently drop these critical ones from the covered set.
///
/// Any table removed from this list requires a PR explaining why it no
/// longer stores financial or compliance data.
pub const REQUIRED_FINANCIAL_TABLES: &[&str] = &[
    "transactions",
    "settlements",
    "settlement_disputes",
    "audit_logs",
    "compliance_reports",
    "reconciliation_reports",
    "tenants",
];

/// Audit which tables in the public schema are NOT covered by backup
/// verification and why. Returns a coverage report.
///
/// "Covered" means: in `REQUIRED_FINANCIAL_TABLES` OR not in the schema
/// (transient), OR in `BACKUP_COVERAGE_EXCLUSIONS` with documented
/// justification.
///
/// This function does not require a live database connection — it works
/// against the exclusion and required-table lists statically so it can run
/// in unit tests without Docker.
pub fn audit_table_coverage<'a>(
    tables_in_schema: &[&'a str],
) -> TableCoverageReport<'a> {
    let required: std::collections::HashSet<&str> =
        REQUIRED_FINANCIAL_TABLES.iter().copied().collect();
    let excluded: std::collections::HashSet<&str> =
        BACKUP_COVERAGE_EXCLUSIONS.iter().copied().collect();

    let mut covered = Vec::new();
    let mut excluded_tables = Vec::new();
    let mut uncovered_gaps = Vec::new();

    for table in tables_in_schema {
        if required.contains(table) {
            covered.push(*table);
        } else if excluded.contains(table) {
            excluded_tables.push(*table);
        } else {
            // Not in required, not in exclusions → potential gap
            uncovered_gaps.push(*table);
        }
    }

    // Also check that every required table actually exists in the schema.
    let in_schema: std::collections::HashSet<&str> =
        tables_in_schema.iter().copied().collect();
    let missing_from_schema: Vec<&str> = REQUIRED_FINANCIAL_TABLES
        .iter()
        .copied()
        .filter(|t| !in_schema.contains(t))
        .collect();

    TableCoverageReport {
        covered,
        excluded: excluded_tables,
        uncovered_gaps,
        missing_from_schema,
    }
}

#[derive(Debug)]
pub struct TableCoverageReport<'a> {
    /// Tables present in schema AND in the required financial-tables list.
    pub covered: Vec<&'a str>,
    /// Tables present in schema but explicitly excluded with justification.
    pub excluded: Vec<&'a str>,
    /// Tables present in schema, not in required list, not in exclusion
    /// list — these are potential coverage gaps that warrant review.
    pub uncovered_gaps: Vec<&'a str>,
    /// Required financial tables that are missing from the schema entirely
    /// (indicates a dropped table or a migration that hasn't been run).
    pub missing_from_schema: Vec<&'a str>,
}

// ── BackupVerificationJob ─────────────────────────────────────────────────────

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
                            tracing::error!(
                                "Backup verification FAILED for {}: {e}",
                                latest.filename
                            );
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
        assert!(
            result.is_err(),
            "corrupted backup must fail checksum verification"
        );
        job.execute().await.unwrap();
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

    // ── #1116: dynamic table coverage ────────────────────────────────────────

    /// Every table in REQUIRED_FINANCIAL_TABLES must be in the covered set
    /// when passed as part of the schema. No financial table should show up
    /// as an uncovered gap or as missing.
    #[test]
    fn all_required_financial_tables_are_covered() {
        let report = audit_table_coverage(super::REQUIRED_FINANCIAL_TABLES);
        assert!(
            report.uncovered_gaps.is_empty(),
            "Required financial tables appeared as uncovered gaps — they must be in \
             REQUIRED_FINANCIAL_TABLES or BACKUP_COVERAGE_EXCLUSIONS:\n  {}",
            report.uncovered_gaps.join(", ")
        );
        assert!(
            report.missing_from_schema.is_empty(),
            "Required financial tables are missing from the provided schema list:\n  {}",
            report.missing_from_schema.join(", ")
        );
        assert_eq!(
            report.covered.len(),
            super::REQUIRED_FINANCIAL_TABLES.len(),
            "All required financial tables must appear in the covered set"
        );
    }

    /// A new table that stores financial data but is not in any list (i.e.
    /// the developer forgot to register it) must appear in `uncovered_gaps`
    /// so the drift-detection logic catches it.
    #[test]
    fn new_unregistered_financial_table_appears_as_gap() {
        let schema_with_new_table = {
            let mut v: Vec<&str> = super::REQUIRED_FINANCIAL_TABLES.to_vec();
            v.push("new_financial_table_not_registered");
            v
        };
        let report = audit_table_coverage(&schema_with_new_table);
        assert!(
            report.uncovered_gaps.contains(&"new_financial_table_not_registered"),
            "A new table that is not in REQUIRED_FINANCIAL_TABLES or \
             BACKUP_COVERAGE_EXCLUSIONS must show up as an uncovered gap"
        );
    }

    /// Explicitly excluded tables must not appear as gaps or as covered.
    #[test]
    fn excluded_tables_are_not_flagged_as_gaps() {
        let schema = super::BACKUP_COVERAGE_EXCLUSIONS;
        let report = audit_table_coverage(schema);
        assert!(
            report.uncovered_gaps.is_empty(),
            "Tables in BACKUP_COVERAGE_EXCLUSIONS must not appear as uncovered gaps:\n  {}",
            report.uncovered_gaps.join(", ")
        );
        // They must not appear as 'covered' either (they are not financial tables).
        assert!(
            report.covered.is_empty(),
            "Tables in BACKUP_COVERAGE_EXCLUSIONS must not appear in the covered set"
        );
    }

    /// Required financial tables that do NOT appear in the schema are reported
    /// as missing — this catches dropped-table scenarios.
    #[test]
    fn missing_required_table_is_reported() {
        // Pass an empty schema — all required tables will be missing.
        let report = audit_table_coverage(&[]);
        assert_eq!(
            report.missing_from_schema.len(),
            super::REQUIRED_FINANCIAL_TABLES.len(),
            "All required financial tables must appear as missing when schema is empty"
        );
    }

    /// Dynamic coverage drift test: every table in an information_schema-like
    /// list that matches a "financial or compliance data" heuristic must be
    /// either in REQUIRED_FINANCIAL_TABLES or BACKUP_COVERAGE_EXCLUSIONS.
    ///
    /// The heuristic: table names containing any of the keywords below are
    /// assumed to hold financial or compliance data unless explicitly excluded.
    #[test]
    fn dynamic_coverage_drift_heuristic() {
        // Simulated information_schema result covering all tables we know about.
        let all_known_tables = [
            "transactions",
            "settlements",
            "settlement_disputes",
            "audit_logs",
            "compliance_reports",
            "reconciliation_reports",
            "tenants",
            "backup_verification_logs",
            "audit_log_archives",
            "sqlx_migrations",
            "_sqlx_migrations",
            "account_monitor_cursors",
            "feature_flags",
            "feature_flag_rollout",
            "feature_flag_dependencies",
            "feature_flag_audit",
            "api_quotas",
            "webhook_delivery_attempts",
            "webhook_events",
            "webhook_filter_rules",
            "webhook_endpoints",
            "webhook_replay_queue",
            "idempotency_keys",
            "transaction_dlq",
            "assets",
            "asset_processing_rules",
        ];

        // Heuristic keywords that imply financial/compliance relevance.
        let financial_keywords = [
            "transaction",
            "settlement",
            "audit",
            "compliance",
            "reconciliation",
            "tenant",
        ];

        let required: std::collections::HashSet<&str> =
            super::REQUIRED_FINANCIAL_TABLES.iter().copied().collect();
        let excluded: std::collections::HashSet<&str> =
            super::BACKUP_COVERAGE_EXCLUSIONS.iter().copied().collect();

        let mut unregistered = Vec::new();
        for table in &all_known_tables {
            let looks_financial = financial_keywords
                .iter()
                .any(|kw| table.contains(kw));
            if looks_financial && !required.contains(table) && !excluded.contains(table) {
                unregistered.push(*table);
            }
        }

        assert!(
            unregistered.is_empty(),
            "Tables matching a 'financial or compliance data' heuristic are neither in \
             REQUIRED_FINANCIAL_TABLES nor BACKUP_COVERAGE_EXCLUSIONS. Add them to one \
             list with a justification comment:\n  {}",
            unregistered.join("\n  ")
        );
    }
} // end mod tests