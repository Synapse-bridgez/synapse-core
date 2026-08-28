# Migration Safety

`scripts/check-migration-safety.sh` is a heuristic pattern-based linter that catches common unsafe migration patterns before they reach production. It is consistent with the approach in the existing codebase: no full SQL semantic analysis, just reliable regex/grep patterns.

## Running

```bash
# Check all migrations (default)
./scripts/check-migration-safety.sh

# Check a specific file
./scripts/check-migration-safety.sh migrations/20260325000000_webhook_endpoints.sql

# Run the full fixture + regression test suite
./scripts/test-migration-safety.sh
```

## Rules

| Rule ID | Pattern Detected | Why It's Unsafe |
|---|---|---|
| `sensitive-type-change` | `ALTER COLUMN … TYPE` without a `USING` clause | Implicit casts can silently truncate or lose data. |
| `not-null-without-default` | `ADD COLUMN … NOT NULL` with no `DEFAULT` | PostgreSQL must rewrite the entire table to fill the new column, taking an `AccessExclusiveLock`. |
| `non-concurrent-index` | `CREATE INDEX` without `CONCURRENTLY` | Blocks all writes on the table until the index build completes. |
| `constraint-without-not-valid` | `ADD CONSTRAINT` without `NOT VALID` | PostgreSQL scans every row to validate the constraint, holding a full lock for the duration. |
| `rename-column` | `RENAME COLUMN` | Any application code still referencing the old column name will break at runtime. |
| `rename-table` | `RENAME TO` (table rename) | Any application code still referencing the old table name will break at runtime. |

## Safe Alternatives

### NOT NULL column without DEFAULT
```sql
-- Bad
ALTER TABLE t ADD COLUMN flag BOOLEAN NOT NULL;

-- Good — supply a DEFAULT so Postgres stores it as metadata (PG 11+)
ALTER TABLE t ADD COLUMN flag BOOLEAN NOT NULL DEFAULT false;
-- Or: add nullable first, backfill, then add constraint
ALTER TABLE t ADD COLUMN flag BOOLEAN;
UPDATE t SET flag = false;
ALTER TABLE t ALTER COLUMN flag SET NOT NULL;
```

### Non-concurrent index
```sql
-- Bad
CREATE INDEX idx_t_col ON t(col);

-- Good
CREATE INDEX CONCURRENTLY idx_t_col ON t(col);
```

### Constraint without NOT VALID
```sql
-- Bad
ALTER TABLE t ADD CONSTRAINT chk_positive CHECK (amount > 0);

-- Good — two-step: add without scanning, validate separately
ALTER TABLE t ADD CONSTRAINT chk_positive CHECK (amount > 0) NOT VALID;
-- In a follow-up migration or low-traffic window:
ALTER TABLE t VALIDATE CONSTRAINT chk_positive;
```

### Column / table rename
```sql
-- Risky — breaks live app code referencing the old name
ALTER TABLE t RENAME COLUMN old_name TO new_name;

-- Preferred approach: add new column, dual-write, then drop old column
ALTER TABLE t ADD COLUMN new_name VARCHAR(50);
-- backfill + update app code to write both columns
-- after full rollout: ALTER TABLE t DROP COLUMN old_name;
```

## Allowlist

Justified exceptions (e.g., indexes on brand-new tables with no live traffic, or a rename that was coordinated with a simultaneous deploy) can be recorded in `scripts/migration-safety-known-index-locks.txt`.

Format: one `<filename>:<rule-id>` entry per line.

```
# Example: initial schema — table is empty at creation time
20250216000000_init.sql:non-concurrent-index
```

Allowlisted entries are logged as `[ALLOWED]` rather than `[ERROR]` and do not count toward the failure total.

## Fixtures

Fixture pairs live under `scripts/fixtures/migration-safety/`. Each directory contains a single `migration.sql`:

- `unsafe-*` directories must be flagged by the linter (test fails if they pass).
- `safe-*` directories must pass the linter cleanly (test fails if they error).

| Fixture | Rule |
|---|---|
| `unsafe-sensitive-type-change` | `sensitive-type-change` |
| `safe-guarded-sensitive-type-change` | `sensitive-type-change` |
| `unsafe-not-null-without-default` | `not-null-without-default` |
| `safe-not-null-with-default` | `not-null-without-default` |
| `unsafe-non-concurrent-index` | `non-concurrent-index` |
| `safe-concurrent-index` | `non-concurrent-index` |
| `unsafe-constraint-without-not-valid` | `constraint-without-not-valid` |
| `safe-constraint-not-valid` | `constraint-without-not-valid` |
| `unsafe-rename-column` | `rename-column` |
| `unsafe-rename-table` | `rename-table` |
