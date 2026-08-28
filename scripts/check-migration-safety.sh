#!/usr/bin/env bash
# Check SQL migration files for potentially unsafe operations.
# Usage: ./scripts/check-migration-safety.sh <migrations-dir>
set -euo pipefail

MIGRATIONS_DIR="${1:?Usage: $0 <migrations-dir>}"
ERRORS=0

# Migrations already applied in production before this check existed (see
# docs/hot-table-index-migrations.md for the incident-correlation
# audit). These are grandfathered as ::notice, not ::warning, so history
# doesn't turn CI red — but the list is closed: new entries are not meant to
# be added here going forward. A genuinely new hot-table index should either
# be reviewed and merged as an accepted risk (documented inline in the
# migration, not added to this file) or built out-of-band per the runbook.
BASELINE_FILE="$(dirname "$0")/migration-safety-known-index-locks.txt"

# Patterns considered unsafe in up migrations (not .down.sql files).
# ALTER COLUMN ... TYPE is handled separately below, not in this list — it
# carries two distinct kinds of risk (lock duration vs. silent data
# corruption) that a single generic warning can't distinguish, and treating
# them as equally low-stakes trains reviewers to skim past both.
UNSAFE_PATTERNS=(
  "DROP TABLE"
  "DROP COLUMN"
  "TRUNCATE"
  "ALTER COLUMN .* SET NOT NULL"
)

# Column names where a TYPE change is more likely to be reinterpreting a
# security-sensitive value (credential, key material, signing secret) than
# doing a benign type widening. Heuristic, not exhaustive — the point is to
# escalate the specific migrations most likely to need a precondition guard
# like migrations/20260824000003_hash_tenant_secrets.sql's, not to catch
# every case.
SENSITIVE_COLUMN_PATTERN='(secret|password|passwd|token|api_key|credential|private_key)'

for file in "$MIGRATIONS_DIR"/*.sql; do
  # Skip down migrations — destructive ops are expected there
  [[ "$file" == *.down.sql ]] && continue
  [[ -f "$file" ]] || continue

  for pattern in "${UNSAFE_PATTERNS[@]}"; do
    if grep -iE "$pattern" "$file" | grep -qvE '^\s*--'; then
      echo "::warning file=$file::Potentially unsafe operation detected: $pattern"
      ERRORS=$((ERRORS + 1))
    fi
  done

  # ALTER COLUMN ... TYPE carries two independent risks that this check
  # reports separately so a reviewer can't mistake one for "just" the other:
  #   - Structural risk: like any ALTER TABLE that rewrites the table, this
  #     can take a long-held lock for the rewrite's duration.
  #   - Data-semantic risk: a USING clause that reinterprets bytes (e.g. a
  #     `::bytea` cast) rather than genuinely transforming them can silently
  #     produce a value the application can no longer correctly decode —
  #     with no error at migration time, only a runtime failure later,
  #     wherever that column is next read.
  # A migration that already enforces its own precondition (e.g. `RAISE
  # EXCEPTION` guarding a `count(*)` check — see
  # migrations/20260824000003_hash_tenant_secrets.sql) has done the thing
  # this check exists to force; it should read as satisfied, not stay red
  # forever. This is deliberately narrow (both an exception raise *and* a
  # count check must be present) so a migration can't silence the warning
  # with an unrelated RAISE EXCEPTION elsewhere in the file.
  has_precondition_guard=false
  if grep -qiE 'RAISE EXCEPTION' "$file" && grep -qiE 'count\s*\(\s*\*\s*\)' "$file"; then
    has_precondition_guard=true
  fi

  while IFS= read -r type_change_line; do
    [[ -z "${type_change_line// }" ]] && continue
    column=$(echo "$type_change_line" | grep -ioE 'ALTER COLUMN[[:space:]]+[a-zA-Z0-9_"]+' | awk '{print $NF}' | tr -d '"')
    if echo "$column" | grep -qiE "$SENSITIVE_COLUMN_PATTERN"; then
      if [[ "$has_precondition_guard" == true ]]; then
        echo "::notice file=$file::ALTER COLUMN \"$column\" ... TYPE on a security-sensitive-looking column, but this file also contains a RAISE EXCEPTION precondition guard (count(*) check) — the data-semantic risk (silent corruption of pre-existing rows) is enforced, not just documented. Structural risk (lock duration) still applies; confirm table size/traffic separately."
      else
        echo "::warning file=$file::ALTER COLUMN \"$column\" ... TYPE on a security-sensitive-looking column. Two separate risks: (1) structural — this can hold a long lock for the rewrite; (2) data-semantic — if the USING clause reinterprets bytes rather than transforming them, existing rows can be silently corrupted with no error until the application next tries to decode them. If this table can have pre-existing rows, add an explicit precondition guard (RAISE EXCEPTION if non-empty, or a real data migration) rather than assuming — see migrations/20260824000003_hash_tenant_secrets.sql for the pattern."
        ERRORS=$((ERRORS + 1))
      fi
    else
      echo "::warning file=$file::ALTER COLUMN \"$column\" ... TYPE detected. Structural risk: this can hold a long lock on the table for the rewrite's duration — confirm the table's size/traffic before merging. Data-semantic risk: if the USING clause reinterprets bytes rather than transforming them, this can silently produce values the application can't correctly decode later. Review both before merging, not just lock duration."
      ERRORS=$((ERRORS + 1))
    fi
  done < <(grep -iE 'ALTER COLUMN .* TYPE' "$file" | grep -vE '^\s*--')

  # CREATE INDEX without CONCURRENTLY takes a write-blocking lock on the
  # target table (per partition, for a partitioned table) for the entire
  # build duration. This is a separate check from UNSAFE_PATTERNS above
  # because it needs to (a) match across the multi-line CREATE INDEX ... ON
  # <table> statements used throughout migrations/, (b) exclude statements
  # that already say CONCURRENTLY, and (c) skip tables created earlier in
  # this same migration — indexing a table with no existing writers yet is
  # not a hot-table lock risk.
  #
  # Note: CREATE INDEX CONCURRENTLY cannot run inside the transaction that
  # sqlx::migrate::Migrator wraps each migration in (Postgres forbids it),
  # so today CONCURRENTLY is not a fix you can apply in-line here — see
  # docs/hot-table-index-migrations.md for the actual process for
  # a table with live traffic. This check exists to force that decision to
  # be made deliberately instead of silently, not to suggest adding the
  # keyword.
  statements=$( (grep -v '^[[:space:]]*--' "$file" || true) | tr '\n' ' ' | tr ';' '\n')

  new_tables=$( (echo "$statements" \
    | grep -ioE 'CREATE TABLE( IF NOT EXISTS)?[[:space:]]+[a-zA-Z0-9_."]+' || true) \
    | awk '{print $NF}' | tr -d '"' | tr '[:upper:]' '[:lower:]' | sort -u)

  while IFS= read -r stmt; do
    [[ -z "${stmt// }" ]] && continue
    if echo "$stmt" | grep -qiE 'CREATE( UNIQUE)?[[:space:]]+INDEX' && ! echo "$stmt" | grep -qi 'CONCURRENTLY'; then
      # Token-based extraction (not grep -o) so that an index name ending in
      # "_on" (e.g. idx_feature_flags_depends_on) can't be mistaken for the
      # "ON <table>" keyword itself.
      table=$(echo "$stmt" | tr -s '[:space:]' '\n' \
        | awk 'tolower($0) == "on" { getline t; print t; exit }' \
        | sed 's/(.*//' | tr -d '"' | tr '[:upper:]' '[:lower:]')

      if [[ -n "$table" ]] && echo "$new_tables" | grep -qxF "$table"; then
        continue
      fi

      file_base="$(basename "$file")"
      if [[ -f "$BASELINE_FILE" ]] && tr -d '\r' < "$BASELINE_FILE" | grep -qxF "${file_base}:${table}"; then
        echo "::notice file=$file::Pre-existing non-concurrent CREATE INDEX on table \"$table\" — grandfathered via scripts/migration-safety-known-index-locks.txt (already applied in production; not counted as a new violation)."
        continue
      fi

      echo "::warning file=$file::Non-concurrent CREATE INDEX detected on table \"$table\" — this holds a write-blocking lock on it for the build's duration. Confirm the table is not already carrying live traffic before merging. See docs/hot-table-index-migrations.md."
      ERRORS=$((ERRORS + 1))
    fi
  done <<< "$statements"

  # Rule: ADD COLUMN NOT NULL without DEFAULT
  # Causes INSERT failures on old app instances that omit the column.
  # Only applies to ALTER TABLE ADD COLUMN, not CREATE TABLE column definitions.
  while IFS= read -r add_col_line; do
    [[ -z "${add_col_line// }" ]] && continue
    if echo "$add_col_line" | grep -qiE '\bDEFAULT\b'; then
      continue
    fi
    if echo "$add_col_line" | grep -qiE '\bGENERATED\b|\bAS\b'; then
      continue
    fi
    echo "::warning file=$file::ADD COLUMN ... NOT NULL without DEFAULT detected. Old app instances that INSERT without this column will fail. Add a DEFAULT or add the column nullable and backfill separately."
    ERRORS=$((ERRORS + 1))
  done < <(grep -v '^[[:space:]]*--' "$file" | grep -iE '\bALTER\s+TABLE\b' | grep -iE 'ADD COLUMN[[:space:]]' | grep -iE '\bNOT[[:space:]]+NULL\b')

  # Rule: RENAME COLUMN
  # Immediately breaks app code referencing the old column name.
  _fb="$(basename "$file")"
  _rename_col_hits=$(grep -iE '\bRENAME\s+COLUMN\b' "$file" | grep -cvE '^\s*--' || true)
  if [[ "$_rename_col_hits" -gt 0 ]]; then
    if [[ -f "$BASELINE_FILE" ]] && tr -d '\r' < "$BASELINE_FILE" | grep -qxF "${_fb}:RENAME_COLUMN"; then
      echo "::notice file=$file::RENAME COLUMN detected — grandfathered via scripts/migration-safety-known-index-locks.txt."
    else
      echo "::warning file=$file::RENAME COLUMN detected. Breaks any running app instance referencing the old column name. Use the expand/contract pattern instead."
      ERRORS=$((ERRORS + 1))
    fi
  fi

  # Rule: RENAME TABLE (ALTER TABLE ... RENAME TO)
  # Immediately breaks app code, views, or triggers referencing the old name.
  _rename_to_hits=$(grep -iE '\bRENAME\s+TO\b' "$file" | grep -cvE '^\s*--' || true)
  if [[ "$_rename_to_hits" -gt 0 ]]; then
    if [[ -f "$BASELINE_FILE" ]] && tr -d '\r' < "$BASELINE_FILE" | grep -qxF "${_fb}:RENAME_TO"; then
      echo "::notice file=$file::ALTER TABLE ... RENAME TO detected — grandfathered via scripts/migration-safety-known-index-locks.txt."
    else
      echo "::warning file=$file::ALTER TABLE ... RENAME TO detected. Breaks any running app instance referencing the old table name. Use a view shim or phased add/migrate/drop pattern instead."
      ERRORS=$((ERRORS + 1))
    fi
  fi

done

if [[ $ERRORS -gt 0 ]]; then
  echo "Found $ERRORS potentially unsafe migration operation(s). Review before merging."
  exit 1
fi

echo "Migration safety check passed."
