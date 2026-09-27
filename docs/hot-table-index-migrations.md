# Adding an Index to a Hot Table

## The problem

`transactions` (and a handful of other tables — see `scripts/migration-safety-known-index-locks.txt`)
carry continuous production write traffic. A plain `CREATE INDEX` takes a
`SHARE` lock on the target table for the entire duration of the index
build, which blocks writes (`INSERT`/`UPDATE`/`DELETE`) on that table until
it completes. On a large or partitioned table, that duration can be minutes,
not seconds — and `transactions` is `PARTITION BY RANGE (created_at)`, so a
plain index build takes that lock on every partition it targets.

`CREATE INDEX CONCURRENTLY` is Postgres's answer to this — it builds the
index without holding a long-lived blocking lock, at the cost of taking
roughly 2-3x longer and being unable to run inside a transaction block.

## Why you can't just add `CONCURRENTLY`

This project's migrations run through `sqlx::migrate::Migrator`
(`src/main.rs`), which wraps each migration file in a single transaction.
Postgres rejects `CREATE INDEX CONCURRENTLY` inside a transaction block
outright:

```
ERROR:  CREATE INDEX CONCURRENTLY cannot run inside a transaction block
```

So today, every `CREATE INDEX` that runs through the normal migration path
is necessarily the blocking kind — `scripts/check-migration-safety.sh`'s
CREATE INDEX rule exists to make sure that's a deliberate decision, not a
missed one, for any table that isn't brand new in the same migration.

## The actual process for indexing a hot table

Until the migration runner supports a non-transactional mode (tracked as a
follow-up; not implemented as part of this change — see "Known gaps" below),
building an index on a table with live traffic is a manual, out-of-band
operation:

1. **Write the index DDL, but do not put it in `migrations/`.** A migration
   file implies "safe to run inside the standard transactional migrator,"
   which this isn't.
2. **Run `CREATE INDEX CONCURRENTLY IF NOT EXISTS ...` directly against the
   target database**, outside of any transaction (`psql` with autocommit,
   not `BEGIN; ... COMMIT;`). Use the exact index name and definition you
   would have put in a migration.
3. **Verify the build succeeded and the index is valid.** A concurrent build
   that fails partway leaves an `INVALID` index behind:
   ```sql
   SELECT indexrelid::regclass, indisvalid
   FROM pg_index
   WHERE indexrelid = 'idx_your_index_name'::regclass;
   ```
   If `indisvalid` is `false`, `DROP INDEX` it and retry — an invalid index
   is dead weight (still maintained on writes, never used by the planner).
4. **Add a no-op migration that matches reality**, so `sqlx migrate run`
   against a fresh database (CI, a new environment) produces the same schema:
   ```sql
   -- This index was built CONCURRENTLY, out-of-band, against production —
   -- see docs/hot-table-index-migrations.md. This migration only makes a
   -- fresh database match that state; CONCURRENTLY is omitted here because
   -- a fresh/CI database has no concurrent writers to block, so the
   -- blocking build is harmless in that context.
   CREATE INDEX IF NOT EXISTS idx_your_index_name ON transactions(...);
   ```
   This is exactly the pattern the six/ten pre-existing entries in
   `scripts/migration-safety-known-index-locks.txt` already are — a plain
   `CREATE INDEX` in the migration file, with the real production build
   having happened separately. Add the new filename:table pair to that file
   only after you've actually done the out-of-band build (it's a record of
   what happened, not a way to skip review).
5. **Get review on the DDL and the target table** before running step 2, not
   after. The plain-migration version in step 4 is retroactive
   documentation, not the mechanism that makes production safe.

## What tables count as "hot"

There's no automated row-count/traffic threshold check today (that would
require querying the live database from a static migration-safety script).
As a rule of thumb, treat any table that isn't created in the same migration
as the index — and specifically `transactions`, `webhook_deliveries`, and
`webhook_delivery_dlq` — as hot until shown otherwise for a specific
low-traffic case.

## Known gaps

- **No non-transactional migration runner mode.** `sqlx::migrate::Migrator`
  wrapping every migration in a transaction is a project-wide constraint,
  not something changed here. The manual process above is the accepted
  workaround; building an actual "non-transactional migration" mode into
  the CLI/migrator is a larger change tracked separately, not part of this
  fix.
- **No historical incident-correlation audit.** The six migrations named in
  the originating issue (plus four more this audit's stricter check also
  found: `20260220000000_settlements.sql`, `20260426000001_feature_flag_rollout.sql`,
  `20260428000002_feature_flag_dependencies.sql`,
  `20260823000004_webhook_delivery_dlq_unique_delivery_id.sql`) were not
  cross-referenced against lock-wait-spike or timeout-error monitoring data
  around their deploy times — this repo checkout has no access to that
  historical monitoring data. Anyone with access to the relevant
  APM/Postgres logs should do that correlation before treating the absence
  of a known incident as evidence there wasn't one.
- **No row-count/traffic threshold.** The check flags all non-concurrent
  `CREATE INDEX` statements against tables not created in the same
  migration, rather than only those above a specific size. This is
  intentionally conservative (see `scripts/check-migration-safety.sh`).
