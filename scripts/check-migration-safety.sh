#!/usr/bin/env bash
# Check SQL migration files for potentially unsafe operations.
# Usage: ./scripts/check-migration-safety.sh <migrations-dir>
set -euo pipefail

MIGRATIONS_DIR="${1:?Usage: $0 <migrations-dir>}"
ERRORS=0

BASELINE_FILE="$(dirname "$0")/migration-safety-known-index-locks.txt"

UNSAFE_PATTERNS=(
  "DROP TABLE"
  "DROP COLUMN"
  "TRUNCATE"
  "ALTER COLUMN .* SET NOT NULL"
)

SENSITIVE_COLUMN_PATTERN='(secret|password|passwd|token|api_key|credential|private_key)'

for file in "$MIGRATIONS_DIR"/*.sql; do
  [[ "$file" == *.down.sql ]] && continue
  [[ -f "$file" ]] || continue

  for pattern in "${UNSAFE_PATTERNS[@]}"; do
    if grep -iE "$pattern" "$file" | grep -qvE '^\s*--'; then
      echo "::warning file=$file::Potentially unsafe operation detected: $pattern"
      ERRORS=$((ERRORS + 1))
    fi
  done

  has_precondition_guard=false
  if grep -qiE 'RAISE EXCEPTION' "$file" && grep -qiE 'count\s*\(\s*\*\s*\)' "$file"; then
    has_precondition_guard=true
  fi

  while IFS= read -r type_change_line; do
    [[ -z "${type_change_line// }" ]] && continue
    column=$(echo "$type_change_line" | grep -ioE 'ALTER COLUMN[[:space:]]+[a-zA-Z0-9_"]+' | awk '{print $NF}' | tr -d '"')
    if echo "$column" | grep -qiE "$SENSITIVE_COLUMN_PATTERN"; then
      if [[ "$has_precondition_guard" == true ]]; then
        echo "::notice file=$file::ALTER COLUMN \"$column\" ... TYPE on a security-sensitive-looking column, but this file also contains a RAISE EXCEPTION precondition guard (count(*) check)."
      else
        echo "::warning file=$file::ALTER COLUMN \"$column\" ... TYPE on a security-sensitive-looking column without a precondition guard. See migrations/20260824000003_hash_tenant_secrets.sql for the pattern."
        ERRORS=$((ERRORS + 1))
      fi
    else
      echo "::warning file=$file::ALTER COLUMN \"$column\" ... TYPE detected. Can hold a long lock and silently corrupt data if USING clause reinterprets bytes."
      ERRORS=$((ERRORS + 1))
    fi
  done < <(grep -iE 'ALTER COLUMN .* TYPE' "$file" | grep -vE '^\s*--')

  statements=$( (grep -v '^[[:space:]]*--' "$file" || true) | tr '\n' ' ' | tr ';' '\n')

  new_tables=$( (echo "$statements" \
    | grep -ioE 'CREATE TABLE( IF NOT EXISTS)?[[:space:]]+[a-zA-Z0-9_."]+' || true) \
    | awk '{print $NF}' | tr -d '"' | tr '[:upper:]' '[:lower:]' | sort -u)

  while IFS= read -r stmt; do
    [[ -z "${stmt// }" ]] && continue
    if echo "$stmt" | grep -qiE 'CREATE( UNIQUE)?[[:space:]]+INDEX' && ! echo "$stmt" | grep -qi 'CONCURRENTLY'; then
      table=$(echo "$stmt" | tr -s '[:space:]' '\n' \
        | awk 'tolower($0) == "on" { getline t; print t; exit }' \
        | sed 's/(.*//' | tr -d '"' | tr '[:upper:]' '[:lower:]')

      if [[ -n "$table" ]] && echo "$new_tables" | grep -qxF "$table"; then
        continue
      fi

      file_base="$(basename "$file")"
      if [[ -f "$BASELINE_FILE" ]] && tr -d '\r' < "$BASELINE_FILE" | grep -qxF "${file_base}:${table}"; then
        echo "::notice file=$file::Pre-existing non-concurrent CREATE INDEX on table \"$table\" -- grandfathered via scripts/migration-safety-known-index-locks.txt."
        continue
      fi

      echo "::warning file=$file::Non-concurrent CREATE INDEX on table \"$table\" -- holds a write-blocking lock. See docs/hot-table-index-migrations.md."
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
      echo "::notice file=$file::RENAME COLUMN detected -- grandfathered via scripts/migration-safety-known-index-locks.txt."
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
      echo "::notice file=$file::ALTER TABLE ... RENAME TO detected -- grandfathered via scripts/migration-safety-known-index-locks.txt."
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
