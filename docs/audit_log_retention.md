# Audit log retention

## What triggers archival

`AuditLogRetentionJob` (`src/services/scheduler.rs`) runs monthly (1st of
the month, 02:00 UTC) via `JobScheduler`. It exports every `audit_logs` row
older than the configured retention window (`AUDIT_LOG_RETENTION_DAYS`,
default 365 days) to a gzip-compressed NDJSON archive, then deletes the
exported rows — except rows belonging to a transaction with status
`'disputed'`, which are archived but never deleted.

## The hard invariant

Rows are deleted **only if** the archive write is confirmed to have landed
on its storage backend (`ArchiveStorage::write` returns `Ok`) and the
corresponding metadata row is recorded in `audit_log_archives`. If either
step fails, `run_retention` (`src/db/audit.rs`) returns `Err` before
touching `audit_logs` — the batch is simply retried on the next scheduled
run, still fully present in the live table. This is enforced in code, not
just documented convention; see
[`retention_skips_deletion_when_archive_write_fails`](../tests/audit_log_test.rs)
for the test that pins this behavior.

## Where archives live today

The only implemented `ArchiveStorage` backend is `LocalDiskArchiveStorage`,
writing to `AUDIT_LOG_ARCHIVE_DIR` (default `/tmp/audit_archives`). **This is
explicitly not durable** in any deployment where application instances run
in ephemeral containers — the archive file (and, since the corresponding
rows have already been deleted, the *only* remaining copy of that data) is
lost the next time that specific container is redeployed, rescheduled, or
restarted.

Wiring a real durable backend (object storage, a dedicated archival
database) is intentionally left as follow-up work: it requires a concrete
deployment decision (which provider, which bucket/credentials mechanism)
that isn't this change's to make unilaterally. `ArchiveStorage` is the
extension point — implement it against the chosen backend and pass it to
`run_retention` in place of `LocalDiskArchiveStorage`; no other code needs
to change. A safe rollout for that follow-up: write to both local disk and
the new backend in parallel for at least one full retention cycle, compare,
and only remove the local-disk write once the durable path is confirmed
reliable.

## Locating a past archive for a compliance request

Query `audit_log_archives`:

```sql
SELECT location, checksum, row_count, covers_from, covers_to, created_at
FROM audit_log_archives
WHERE covers_from <= $requested_date AND covers_to >= $requested_date
ORDER BY created_at DESC;
```

`location` is backend-specific (a local path today; a bucket/key URI once a
durable backend is wired in). `checksum` is the SHA-256 of the archived
bytes as written, for verifying the archive hasn't been altered since.

## Metrics

- `audit_archive_write_total{result="success"|"failure"}` — a `failure`
  here means that run's deletion was skipped per the hard invariant above.
- Alert on any `failure` — since deletion is always skipped on failure,
  there's no data-loss risk from an alert firing late, but a `failure` does
  mean `audit_logs` keeps growing until the next successful run.
- `audit_archive_rows_total` and archive metadata checks should be reviewed
  alongside any cold-storage rollout so archive completeness can be verified
  before local-disk retention is reduced.

## Known gap

No durable storage backend is implemented by default — see "Where archives
live today" above. Until one is wired in, this job is safer than before
(it will never silently lose data by deleting rows whose archive didn't
land), but the archive itself is still only as durable as local disk on
whichever container happens to run the job that month.
