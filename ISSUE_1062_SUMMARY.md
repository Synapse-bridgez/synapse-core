# Issue #1062: Four Independent Silent-Correctness Bugs in Admin Tooling

## Summary

This PR fixes four independent Time-of-Check-to-Time-of-Use (TOCTOU) race conditions and a fifth concurrent-restore bug across admin/background operations, where unlocked reads followed by unconditional writes could silently produce incorrect results.

## Parts Fixed

### Part A: Webhook Replay TOCTOU Race

**File**: `src/handlers/admin/webhook_replay.rs`

**Problem**: Admin webhook replay could revert a `completed` transaction back to `pending` if `process_batch` completed it between the replay's validation check and its unconditional `UPDATE`.

**Fix**:
- Wrapped read/write in a single database transaction
- Added `FOR UPDATE` row lock on status read
- Added `WHERE status = $2` guard on the `UPDATE`
- Returns `409 Conflict` if status changes during the operation
- Added `webhook_replay_blocked_total` metric

**Impact**: Prevents duplicate processing and misleading audit trails.

---

### Part B: DLQ Requeue TOCTOU Race

**File**: `src/services/transaction_processor.rs`

**Problem**: Same TOCTOU pattern as Part A — `requeue_dlq` validated a transition against a stale read, then wrote unconditionally.

**Fix**:
- Applied same transactional locking pattern as Part A
- Added `dlq_requeue_blocked_total` metric
- Cross-referenced with issue #1's Part D (dead code with same bug)

**Impact**: Prevents silent bypass of state-machine validation.

---

### Part C: Reconciliation Report Discrepancy Flag Never Written

**Files**: 
- `src/services/reconciliation.rs`
- `src/handlers/admin/reconciliation.rs`

**Problem**: 
1. `has_discrepancies` column always defaulted to `false` — never explicitly set
2. Admin endpoint returned a fabricated UUID instead of the real database-assigned ID
3. Partial index on `has_discrepancies = true` was permanently empty

**Fix**:
- Compute `has_discrepancies` from actual discrepancy counts
- Bind it to the `INSERT` statement
- Add `RETURNING id` clause and return real database-assigned ID
- Admin endpoint uses real ID instead of `Uuid::new_v4()`
- Added `reconciliation_discrepancies_total` metric

**Impact**: Makes discrepancy reports findable via the index; fixes `GET /admin/reconciliation/reports/:id` 404s.

---

### Part D: Settlement Schedule Ignored

**Files**:
- `src/db/models.rs`
- `src/services/settlement.rs`

**Problem**: 
1. `settlement_schedule` column added to database but never read by code
2. `Asset` model had no `settlement_schedule` field
3. All assets settled hourly regardless of configured `daily`/`weekly` cadence
4. Doc comment described the feature but code didn't implement it

**Fix**:
- Added `settlement_schedule` field to `Asset` model
- Updated `Asset::fetch_all` query to include new columns
- Implemented schedule gating logic in `run_settlements()`:
  - `hourly`: always eligible
  - `daily`: only eligible during hour 0 (00:00-01:00 UTC)
  - `weekly`: only eligible on Mondays during hour 0
- Added `settlements_run_total` and `settlements_skipped_total` metrics

**Impact**: Honors configured settlement cadence; can reduce settlement batches by up to 168x for weekly assets.

**Deployment Note**: This changes real settlement timing. See `docs/admin-race-conditions.md` for recommended rollout strategy (shadow mode first).

---

### Part E: PITR Concurrent Restore Guard

**File**: `src/services/pitr.rs`

**Problem**: No check for existing running restore job — two admin requests could spawn two concurrent `pg_basebackup` processes against the same target database.

**Fix**:
- Check for `status = 'running'` restore job before submitting new one
- Return descriptive error with existing job ID if found
- Added `pitr_restore_rejected_concurrent_total` metric

**Impact**: Prevents destructive concurrent physical restores.

---

## Error Handling Changes

**File**: `src/error.rs`

Added `AppError::Conflict` variant:
```rust
#[error("Conflict: {0}")]
Conflict(String),
```

Maps to HTTP 409 status code, used when optimistic locking detects concurrent modification.

---

## Metrics Changes

**File**: `src/metrics.rs`

Made `meter()` function public so ad-hoc metrics can be created throughout the codebase:

```rust
pub fn meter() -> &'static Meter
```

New metrics added:
- `webhook_replay_blocked_total` — Counter
- `dlq_requeue_blocked_total` — Counter
- `reconciliation_discrepancies_total` — Counter
- `settlements_run_total` — Counter
- `settlements_skipped_total` — Counter
- `pitr_restore_rejected_concurrent_total` — Counter

---

## Documentation

**File**: `docs/admin-race-conditions.md`

Comprehensive runbook covering:
- What each bug was and how it was fixed
- Runbooks for reviewing suspicious audit trails
- How to handle historical data (discrepancy flags, settlement schedules)
- Recommended deployment strategy for Part D (settlement schedule enforcement)
- How to handle concurrent restore rejections

---

## Tests

New integration test files (all marked `#[ignore]` — require live Postgres + Redis):

1. **`tests/webhook_replay_concurrency_test.rs`** (Part A)
   - Races webhook replay against `process_batch` completion
   - Verifies replay is blocked with `Conflict` error
   - Verifies transaction remains `completed`, not reverted to `pending`
   - Tests metric incrementation

2. **`tests/dlq_requeue_concurrency_test.rs`** (Part B)
   - Races DLQ requeue against concurrent status change
   - Verifies requeue is blocked when status changes
   - Tests both invalid transitions and concurrent modifications
   - Verifies successful requeue removes DLQ entry

3. **`tests/reconciliation_report_discrepancy_test.rs`** (Part C)
   - Verifies `has_discrepancies` flag correctly computed and stored
   - Verifies real database ID is returned (not fabricated UUID)
   - Verifies reports with discrepancies are findable via partial index
   - Tests admin endpoint returns fetchable report ID

4. **`tests/settlement_schedule_test.rs`** (Part D)
   - Tests hourly schedule (always eligible)
   - Tests daily schedule (only hour 0)
   - Tests weekly schedule (only Monday hour 0)
   - Tests unknown schedule defaults to hourly
   - Verifies metrics are incremented

5. **`tests/pitr_concurrent_restore_test.rs`** (Part E)
   - Tests concurrent restore submission is rejected
   - Verifies descriptive error includes running job ID
   - Tests restore allowed after previous job completes
   - Tests metric incrementation

---

## Breaking Changes

None — all fixes are internal correctness improvements. The settlement schedule enforcement (Part D) changes timing for non-hourly assets, but this is a fix for a feature that never worked as documented.

---

## Migration Notes

### Settlement Schedule Enforcement (Part D)

Before deploying, audit existing `settlement_schedule` configuration:

```sql
SELECT asset_code, settlement_schedule, enabled
FROM assets
ORDER BY asset_code;
```

If any asset has `daily` or `weekly`, confirm with the asset owner that they actually want reduced frequency. If not:

```sql
UPDATE assets 
SET settlement_schedule = 'hourly' 
WHERE asset_code = 'ASSET_CODE';
```

### Historical Reconciliation Reports (Part C)

Historical reports (before this fix) have unreliable `has_discrepancies` flags. To audit:

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

**Do not backfill** without coordinating with the team — see docs for details.

---

## Checklist

- [x] All code changes scoped strictly to issue requirements
- [x] Error handling for race conditions (optimistic locking)
- [x] Metrics for monitoring blocked operations
- [x] Comprehensive documentation and runbooks
- [x] Integration tests for each concurrent scenario
- [x] No unrelated changes or stray files
- [x] Follows existing code patterns and naming conventions

---

## Related Issues

- Issue #1 Part D: Documents the same TOCTOU bug pattern in dead code (`CompleteStage`)
- Issue #7: Clarifies that `replayDlq` GraphQL mutation is unreachable (Part B only affects REST endpoint)

Closes #1062
