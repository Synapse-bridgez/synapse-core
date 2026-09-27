# Backup verification

## What runs, and when

`BackupVerificationJob` (`src/services/backup_verification_job.rs`) runs
weekly (Sunday 2 AM, per its cron schedule) via `JobScheduler`. It lists
backups in `BACKUP_DIR` (see `BackupService`), takes the most recent one, and
calls `BackupService::verify_backup_checksum`, which recomputes the backup
file's SHA-256 and compares it against the checksum recorded in that
backup's `.meta` sidecar at creation time.

**This is checksum-only verification — it does not restore the backup
anywhere.** See [ADR-006](adr/006-backup-verification-checksum-only.md) for
why: `BackupService`'s only restore path always targets the same database
URL the live application uses, and there is no isolated/scratch database
target in this codebase to restore into instead. A checksum mismatch tells
you the backup *file* was corrupted at rest; it does not prove the backup
would successfully restore.

## Where backups live today

Backups are written to local disk at `BACKUP_DIR` (default `./backups`) by
`BackupService::create_backup`. There is no remote/durable copy — if you're
looking for an off-host backup story, it doesn't exist yet in this codebase
as of this change; treat any documentation elsewhere that assumes S3 or
similar remote storage as aspirational rather than current behavior.

## Outcome signals

- `backup_verification_total{result="success"|"failure"|"no_backups"}` —
  counter, incremented once per run.
- `backup_verification_duration_ms` — histogram of run duration.
- Log lines: `"Backup verification succeeded: ..."` /
  `"Backup verification FAILED for {filename}: {error}"` /
  `"No backups found for verification"`.

## What to do when a run reports a backup as corrupted

1. Check `backup_verification_total{result="failure"}` and the job's log
   line for the failing filename and checksum mismatch details.
2. Confirm this isn't a false positive from the verification harness itself:
   re-run `BackupService::verify_backup_checksum` manually against the same
   file and metadata; a transient disk read error would not reproduce.
3. If it reproduces, treat the most recent verified-good backup as your
   actual recovery point and follow
   [the disaster recovery runbook](disaster-recovery.md) — do not assume the
   corrupted backup is usable.
4. File an incident: a corrupted backup at rest usually means either disk
   issues on the host writing backups, or a bug in `create_backup`'s
   compress/encrypt pipeline — both worth investigating beyond just
   discarding the one bad file.

## Known gap

There is currently no on-demand / manual-trigger path (admin endpoint or CLI
command) to run verification outside the weekly schedule. The CLI's backup
subcommands (`synapse-core backup run|list|restore|cleanup`) are stubs today
(`anyhow::bail!("Backup service not yet implemented")` in `src/cli.rs`) —
wiring those up, and adding a manual-trigger-first rollout phase, is a
reasonable follow-up but is out of scope for this change.

## Table coverage (#1116)

### What "covered" means

A table is *covered* when it appears in `REQUIRED_FINANCIAL_TABLES` in
`src/services/backup_verification_job.rs`. These tables are audited at the
module level: the `audit_table_coverage` function (and the unit tests that
call it) will fail if any required table disappears from the schema, or if a
new table that looks financial/compliance-relevant is added to migrations
without being registered in one of the two lists.

### Required financial tables (must always be covered)

| Table | Why |
|---|---|
| `transactions` | Primary financial ledger |
| `settlements` | Settlement batch records |
| `settlement_disputes` | Dispute tracking, financial impact |
| `audit_logs` | Compliance-critical tamper-evidence trail |
| `compliance_reports` | Regulatory reporting records |
| `reconciliation_reports` | Cross-system reconciliation audit trail |
| `tenants` | Identity/registry — compromise would break all tenant isolation |

### Explicitly excluded tables

Tables in `BACKUP_COVERAGE_EXCLUSIONS` are intentionally not treated as gaps.
Each exclusion has a documented justification in the source. Summary:

| Table | Justification |
|---|---|
| `backup_verification_logs` | Infrastructure metadata written *by* the backup process — circular |
| `audit_log_archives` | Retention metadata only; actual audit data is in `audit_logs` |
| `sqlx_migrations` | sqlx internal table, no tenant/financial data |
| `account_monitor_cursors` | Transient cursor state, not a financial record |
| `feature_flags` / `feature_flag_*` | Configuration metadata, not financial data |
| `api_quotas` | Rate-limiting config, not a financial record |
| `webhook_*` / `idempotency_keys` / `transaction_dlq` | Operational plumbing; source-of-truth lives in `transactions` |

### Drift prevention

The unit tests in `src/services/backup_verification_job.rs` (`#[cfg(test)]`)
run fully offline (no Docker, no database) and enforce:

1. All tables in `REQUIRED_FINANCIAL_TABLES` must be covered.
2. A new table added to migrations that matches a financial-data heuristic
   (name contains `transaction`, `settlement`, `audit`, `compliance`,
   `reconciliation`, or `tenant`) **will fail
   `dynamic_coverage_drift_heuristic`** unless it is registered in one of the
   two lists with a code-review comment justifying the decision.

This means a developer cannot add a new financial table in a migration and
have it silently go unverified — the CI test will fail and require a
conscious coverage decision.
