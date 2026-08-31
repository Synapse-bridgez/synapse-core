# ADR-006: Backup verification is checksum-only, not restore-and-verify

## Status

Accepted

## Context

The weekly `BackupVerificationJob` is meant to be the safety net that catches
a corrupted or unrestorable backup before it's needed in a real emergency.
Prior to this change the job was a no-op: it listed backups and logged that
the latest one "is available for restore verification" without ever
checksumming or restoring it.

The obvious fix — call `BackupService::restore_backup` from the job — is
unsafe as written. `restore_backup`'s `run_pg_restore` step always targets
`self.database_url`, and `BackupService` is constructed with the same
`DATABASE_URL` the rest of the application uses. There is no isolated,
disposable database target anywhere in this codebase to restore into. A
weekly scheduled job that called `restore_backup` would run a full logical
`psql` restore against the live production database every week — at best a
script full of duplicate-key errors; at worst, depending on how far `psql`
gets before erroring, a real risk to the live dataset.

## Decision

`BackupVerificationJob::execute` calls a new `BackupService::verify_backup_checksum`
method that loads the backup's `.meta` sidecar, recomputes the backup file's
checksum, and compares — the same integrity check `restore_backup` already
performs internally before restoring, extracted so it can run standalone.
This never decrypts, decompresses, restores, or touches any database.

## Consequences

### Positive

- Proves the backup *file* wasn't corrupted at rest, safely, on an
  unattended weekly schedule, with zero risk of touching production data.
- No new infrastructure (scratch database provisioning/teardown) required
  to ship a real fix for the no-op.
- The check runs in milliseconds instead of the minutes a full
  dump-decrypt-decompress-restore cycle would take.

### Negative

- Does not prove the backup is actually *restorable* — a checksum can
  match while the backup is still unusable for reasons a checksum can't
  catch (e.g. a `pg_dump` version incompatibility with the target
  `psql`, or an internal SQL syntax issue that never corrupted the file
  bytes). This is the real gap this ADR accepts as a known limitation.
- If Postgres ever expects a full restore-and-checksum, it isn't provided
  by this task.

### Neutral

- Reuses the existing `.meta`-sidecar checksum embedded at backup-creation
  time; introduces no new stored data.

## Alternatives Considered

### Alternative 1: Full restore-and-checksum against a scratch database

**Description:** Provision an isolated, disposable Postgres instance (or
reuse a dedicated, clearly-labeled verification database via a new config
value distinct from `DATABASE_URL`), restore the backup into it, checksum
the *restored* data, then tear the scratch target down.

**Pros:**
- Proves the backup is genuinely restorable, not just byte-identical to
  what was written.
- Catches restore-time failures (version skew, corrupted SQL) that a
  file-level checksum cannot.

**Cons:**
- Scratch-database provisioning/teardown is deployment-topology-specific
  (Docker? RDS snapshot restore? a Kubernetes job?) and is a substantial
  piece of infrastructure on its own, including handling the failure path
  where teardown itself fails (a leaked scratch DB).
- A full restore cycle is materially more expensive to run on every
  scheduled invocation than a checksum.

**Why rejected (for now):** The no-op bug is the more urgent problem, and
fixing it safely doesn't require this infrastructure. Checksum-only is a
strict improvement over the previous no-op and ships without a new,
deployment-specific subsystem. This alternative is the natural follow-up
once a target environment for scratch databases is decided.

## Implementation Notes

- `verify_backup_checksum` is `pub` on `BackupService` (the underlying
  `verify_backup` it wraps stays private — it's `restore_backup`'s
  internal helper).
- `BackupVerificationJob::execute` records `backup_verification_total`
  (labeled `result`: `success` | `failure` | `no_backups`) and
  `backup_verification_duration_ms` on every run.
- The job's own doc comment on `BackupVerificationJob` restates this
  tradeoff so a future reader doesn't have to find this ADR to understand
  why the job doesn't restore.

## References

- `src/services/backup_verification_job.rs`
- `src/services/backup.rs` (`verify_backup_checksum`, `restore_backup`)

## Addendum: Coexistence with DR-Drill Restore Testing (Issue 45)

The DR drill introduced in issue 45 performs a full restore-and-verify
against a scratch target — i.e. it is exactly "Alternative 1" above,
implemented, but as an on-demand/periodic drill rather than the every-run
weekly job. This does not amend or contradict the decision in this ADR:

- The **weekly `BackupVerificationJob`** stays checksum-only. Its job is
  cheap, frequent, unattended proof that backup *files* aren't corrupted
  at rest, per the Decision above.
- The **DR drill** covers the gap this ADR names in Consequences/Negative
  (checksum-only "does not prove the backup is actually *restorable*").
  It answers that question on its own cadence, using its own isolated
  target, without changing what the weekly job does.

Net effect: two verification guarantees intentionally coexist at
different frequencies and cost points — checksum-only (weekly, cheap,
file-level) and full restore-test (drill cadence, expensive, proves
restorability). Cold-storage tiering (issue 39) does not change this
split; tiered/archived backups are still subject to the same
checksum-only weekly check plus periodic drill coverage, not a third
policy. Should DR-drill frequency ever increase to the point of
overlapping the weekly job's role, this addendum — not silent drift —
is where that tradeoff should be revisited.
