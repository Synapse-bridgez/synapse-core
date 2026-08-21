# Admin Tooling Race Condition Prevention

This document describes the race condition fixes implemented in issue #1062 and provides runbooks for reviewing suspicious status-change histories.

## Overview

Four independent admin/background operations had Time-of-Check-to-Time-of-Use (TOCTOU) race conditions where an unlocked read followed by an unconditional write could silently overwrite concurrent status changes:

1. **Webhook Replay** (`POST /admin/webhooks/:id/replay`)
2. **DLQ Requeue** (`POST /admin/dlq/:id/requeue`)
3. **Reconciliation Report Storage** (background job + admin trigger)
4. **Settlement Scheduling** (background job)
5. **PITR Restore** (`POST /admin/pitr/restore`)

All have been fixed with transactional locking, guarded writes, and metrics for blocked operations.

## Fixed Patterns

### Webhook Replay Race (Part A)

**Before:** Unlocked read of transaction status, validation check, then unconditional `UPDATE` that could overwrite a concurrent `completed` status written by `process_batch`.

**After:**
- Wrapped in a database transaction
- Row locked with `FOR UPDATE`
- Update includes `WHERE status = $2` guard
- Returns `409 Conflict` if status changed during replay
- Metric: `webhook_replay_blocked_total`

**Runbook: Reviewing Suspicious Replay Audit Trails**

If you see a transaction with a `completed → pending → completed` pattern in audit logs:

1. Check `audit_logs` for the transaction:
   ```sql
   SELECT created_at, action, old_data, new_data, actor
   FROM audit_logs
   WHERE entity_id = '<transaction_id>'
   ORDER BY created_at;
   ```

2. Look for `webhook_replayed` actions between two `status_changed` events

3. If the gap is <5 seconds, this likely indicates the race condition (now fixed)

4. Check if the transaction was processed twice by looking for duplicate settlement/payout records

5. If found, file an incident report with:
   - Transaction ID
   - Audit log timeline
   - Any duplicate downstream effects

### DLQ Requeue Race (Part B)

**Before:** Read current status, validate transition, unconditional `UPDATE` to `pending`.

**After:**
- Same transactional lock pattern as webhook replay
- Metric: `dlq_requeue_blocked_total`

**Runbook: Same as Part A above** — look for unexpected status transitions around DLQ requeue operations.

### Reconciliation Report Discrepancy Flag (Part C)

**Before:**
- `has_discrepancies` column always defaulted to `false` — never written
- Admin trigger returned a fabricated UUID instead of the real database-assigned ID

**After:**
- `has_discrepancies` correctly computed and stored
- Admin endpoint returns real database ID from `RETURNING` clause
- Partial index on `has_discrepancies = true` now actually useful
- Metric: `reconciliation_discrepancies_total`

**Runbook: Reviewing Historical Discrepancy Data**

Historical reports (before this fix) have unreliable `has_discrepancies` flags. To audit:

1. Query reports with `has_discrepancies = false` but non-zero counts:
   ```sql
   SELECT id, generated_at, 
          missing_on_chain_count, 
          orphaned_payments_count,
          amount_mismatches_count
   FROM reconciliation_reports
   WHERE has_discrepancies = false
     AND (missing_on_chain_count > 0 
          OR orphaned_payments_count > 0 
          OR amount_mismatches_count > 0)
   ORDER BY generated_at DESC;
   ```

2. For each row, the `report_json` column contains the full details — you can recompute the flag if needed

3. **Do not attempt to backfill** the `has_discrepancies` column for historical data without coordinating with the team — queries that rely on the partial index may need to be updated first

### Settlement Schedule Enforcement (Part D)

**Before:**
- `settlement_schedule` column added to `assets` table but never read
- All assets settled hourly regardless of configured `daily` or `weekly` schedule
- Doc comment described the feature but code didn't implement it

**After:**
- `Asset` model includes `settlement_schedule` field
- `run_settlements()` checks schedule and skips ineligible assets
- Metrics: `settlements_run_total`, `settlements_skipped_total`

**Schedule Rules:**
- `hourly`: Always eligible
- `daily`: Only eligible during hour 0 (00:00-01:00 UTC)
- `weekly`: Only eligible on Mondays during hour 0

**Runbook: Enabling Schedule Enforcement**

This fix changes real settlement timing. Before deploying:

1. Audit current `settlement_schedule` configuration:
   ```sql
   SELECT asset_code, settlement_schedule, enabled
   FROM assets
   ORDER BY asset_code;
   ```

2. If any asset has `settlement_schedule = 'weekly'` or `'daily'`, confirm with the asset owner that they actually want reduced frequency

3. **Recommended rollout:**
   - Deploy with a dry-run/shadow mode first (log what would be skipped without actually skipping)
   - Monitor `settlements_skipped_total` metric for 1 week
   - Compare skipped count vs. expected schedule
   - Enable enforcement after confirming metric matches expectations

4. If an asset is incorrectly scheduled, update it:
   ```sql
   UPDATE assets 
   SET settlement_schedule = 'hourly' 
   WHERE asset_code = 'ASSET_CODE';
   ```

### PITR Concurrent Restore Guard (Part E)

**Before:** No check for existing running restore — two admin requests could spawn two concurrent `pg_basebackup` processes against the same target.

**After:**
- Check for `status = 'running'` restore job before submitting new one
- Return error with existing job ID if found
- Metric: `pitr_restore_rejected_concurrent_total`

**Runbook: Handling Concurrent Restore Rejection**

If an admin receives:
```
Error: A restore job is already running (job_id: <uuid>). 
Only one restore can run at a time. Please wait for it to complete.
```

1. Check the status of the running job:
   ```sql
   SELECT id, target_timestamp, status, started_at, requested_by
   FROM pitr_restore_jobs
   WHERE id = '<uuid>';
   ```

2. If the job has been running for >4 hours, check the restore process manually:
   ```bash
   ps aux | grep pg_basebackup
   ```

3. If the process is stuck or failed without updating the job status, manually mark it as failed:
   ```sql
   UPDATE pitr_restore_jobs
   SET status = 'failed',
       error_message = 'Manually failed - process stuck',
       completed_at = NOW()
   WHERE id = '<uuid>';
   ```

4. Then retry the restore request

## Metrics Summary

All new metrics for monitoring race condition prevention:

| Metric | Type | Description |
|--------|------|-------------|
| `webhook_replay_blocked_total` | Counter | Replays blocked due to concurrent status change |
| `dlq_requeue_blocked_total` | Counter | DLQ requeues blocked due to concurrent status change |
| `reconciliation_discrepancies_total` | Counter | Reconciliation reports with discrepancies detected |
| `settlements_run_total` | Counter | Number of settlements executed |
| `settlements_skipped_total` | Counter | Settlements skipped due to schedule gating |
| `pitr_restore_rejected_concurrent_total` | Counter | PITR restores rejected due to concurrent job |

All are exposed via the existing OpenTelemetry metrics endpoint.

## Testing Race Conditions

Each fix includes a concurrency test that races the admin operation against the live background process. See:

- `tests/webhook_replay_concurrency_test.rs`
- `tests/dlq_requeue_concurrency_test.rs`
- `tests/reconciliation_report_discrepancy_test.rs`
- `tests/settlement_schedule_test.rs`
- `tests/pitr_concurrent_restore_test.rs`

Run with:
```bash
cargo test --test webhook_replay_concurrency_test -- --ignored
cargo test --test dlq_requeue_concurrency_test -- --ignored
cargo test --test reconciliation_report_discrepancy_test -- --ignored
cargo test --test settlement_schedule_test -- --ignored
cargo test --test pitr_concurrent_restore_test -- --ignored
```

(The `--ignored` flag is required because these tests need live Postgres + Redis.)
