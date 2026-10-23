#!/usr/bin/env bash
# Check SQL migration files for potentially unsafe operations.
# Usage: ./scripts/check-migration-safety.sh <migrations-dir>
set -euo pipefail

MIGRATIONS_DIR="${1:?Usage: $0 <migrations-dir>}"
ERRORS=0

BASELINE_FILE="$(dirname "$0")/migration-safety-known-index-locks.txt"

# Dated exemptions recorded in the expand/contract policy document.
# Format (one per line, inside a fenced code block or plain lines):
#   EXEMPTION: <migration-file>:<RULE_ID> expires=YYYY-MM-DD reason=<text>
POLICY_DOC="$(dirname "$0")/../docs/expand-contract-migrations.md"

# Swap engine tables that are expected to grow large / be high-volume.
SWAP_TABLE_PATTERN='(swap_)?(orders|quotes|fills|events|screenings|submissions)'

# Returns 0 if an unexpired exemption exists for <file-base>:<RULE_ID>.
has_exemption() {
  local file_base="$1" rule_id="$2"
  [[ -f "$POLICY_DOC" ]] || return 1
  local today
  today="$(date -u +%Y-%m-%d)"
  while IFS= read -r line; do
    [[ "$line" == *"EXEMPTION:"* ]] || continue
    local key expires
    key=$(echo "$line" | sed -E 's/.*EXEMPTION:[[:space:]]*([^[:space:]]+).*/\1/')
    expires=$(echo "$line" | sed -nE 's/.*expires=([0-9]{4}-[0-9]{2}-[0-9]{2}).*/\1/p')
    [[ "$key" == "${file_base}:${rule_id}" ]] || continue
    if [[ -z "$expires" ]]; then
      echo "::warning file=$POLICY_DOC::Exemption for ${key} has no expires=YYYY-MM-DD date; ignoring."
      continue
    fi
    if [[ "$expires" < "$today" ]]; then
      echo "::warning file=$POLICY_DOC::Exemption for ${key} expired on ${expires}; ignoring."
      continue
    fi
    echo "::notice file=$file_base::${rule_id} exempted until ${expires} via docs/expand-contract-migrations.md."
    return 0
  done < "$POLICY_DOC"
  return 1
}

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
    elif has_exemption "$_fb" "RENAME_COLUMN"; then
      :
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
    elif has_exemption "$_fb" "RENAME_TO"; then
      :
    else
      echo "::warning file=$file::ALTER TABLE ... RENAME TO detected. Breaks any running app instance referencing the old table name. Use a view shim or phased add/migrate/drop pattern instead."
      ERRORS=$((ERRORS + 1))
    fi
  fi

  # Rule: DROP COLUMN on swap tables
  # Breaks running app instances that still SELECT/INSERT the column.
  # Must only happen in the contract phase, after the app no longer references it.
  while IFS= read -r drop_col_line; do
    [[ -z "${drop_col_line// }" ]] && continue
    table=$(echo "$drop_col_line" | grep -ioE 'ALTER[[:space:]]+TABLE[[:space:]]+[a-zA-Z0-9_."]+' | awk '{print $NF}' | tr -d '"' | tr '[:upper:]' '[:lower:]')
    if echo "$table" | grep -qiE "$SWAP_TABLE_PATTERN"; then
      if has_exemption "$_fb" "DROP_COLUMN_SWAP"; then
        continue
      fi
      echo "::warning file=$file::DROP COLUMN on swap table \"$table\" detected. Only allowed in the contract phase after the application no longer references the column. Record a dated exemption in docs/expand-contract-migrations.md if unavoidable."
      ERRORS=$((ERRORS + 1))
    fi
  done < <(grep -v '^[[:space:]]*--' "$file" | grep -iE '\bALTER\s+TABLE\b' | grep -iE '\bDROP\s+COLUMN\b')

  # Rule: DROP TABLE on swap tables
  # Destroys data still needed by running replicas during rollout.
  while IFS= read -r drop_tbl_line; do
    [[ -z "${drop_tbl_line// }" ]] && continue
    table=$(echo "$drop_tbl_line" | grep -ioE 'DROP[[:space:]]+TABLE([[:space:]]+IF[[:space:]]+EXISTS)?[[:space:]]+[a-zA-Z0-9_."]+' | awk '{print $NF}' | tr -d '"' | tr '[:upper:]' '[:lower:]')
    if echo "$table" | grep -qiE "$SWAP_TABLE_PATTERN"; then
      if has_exemption "$_fb" "DROP_TABLE_SWAP"; then
        continue
      fi
      echo "::warning file=$file::DROP TABLE on swap table \"$table\" detected. Destructive changes require an explicit, dated exemption in docs/expand-contract-migrations.md reviewed in the PR."
      ERRORS=$((ERRORS + 1))
    fi
  done < <(grep -v '^[[:space:]]*--' "$file" | grep -iE '\bDROP\s+TABLE\b')

  # Rule: ADD COLUMN NOT NULL without DEFAULT on swap tables
  # Old replicas that INSERT without the column will fail during rollout.
  while IFS= read -r add_col_line; do
    [[ -z "${add_col_line// }" ]] && continue
    table=$(echo "$add_col_line" | grep -ioE 'ALTER[[:space:]]+TABLE[[:space:]]+[a-zA-Z0-9_."]+' | awk '{print $NF}' | tr -d '"' | tr '[:upper:]' '[:lower:]')
    echo "$table" | grep -qiE "$SWAP_TABLE_PATTERN" || continue
    if has_exemption "$_fb" "ADD_NOT_NULL_SWAP"; then
      continue
    fi
    echo "::warning file=$file::ADD COLUMN ... NOT NULL without DEFAULT on swap table \"$table\". Expand phase must add the column nullable (or with a DEFAULT) and backfill separately."
    ERRORS=$((ERRORS + 1))
  done < <(grep -v '^[[:space:]]*--' "$file" | grep -iE '\bALTER\s+TABLE\b' | grep -iE 'ADD COLUMN[[:space:]]' | grep -iE '\bNOT[[:space:]]+NULL\b' | grep -viE '\bDEFAULT\b')

  # Rule: long-running locks on large swap tables
  # SET NOT NULL / ADD CONSTRAINT ... CHECK / VALIDATE-less constraints take
  # ACCESS EXCLUSIVE or long SHARE locks and block writes on high-volume tables.
  while IFS= read -r lock_line; do
    [[ -z "${lock_line// }" ]] && continue
    table=$(echo "$lock_line" | grep -ioE 'ALTER[[:space:]]+TABLE[[:space:]]+[a-zA-Z0-9_."]+' | awk '{print $NF}' | tr -d '"' | tr '[:upper:]' '[:lower:]')
    echo "$table" | grep -qiE "$SWAP_TABLE_PATTERN" || continue
    if has_exemption "$_fb" "LONG_LOCK_SWAP"; then
      continue
    fi
    echo "::warning file=$file::Long-running lock on large swap table \"$table\" detected. Split into ADD CONSTRAINT ... NOT VALID followed by VALIDATE CONSTRAINT, or SET NOT NULL after a validated CHECK."
    ERRORS=$((ERRORS + 1))
  done < <(grep -v '^[[:space:]]*--' "$file" | grep -iE '\bALTER\s+TABLE\b' | grep -iE 'SET[[:space:]]+NOT[[:space:]]+NULL|ADD[[:space:]]+CONSTRAINT' | grep -viE 'NOT[[:space:]]+VALID')

done

if [[ $ERRORS -gt 0 ]]; then
  echo "Found $ERRORS potentially unsafe migration operation(s). Review before merging."
  exit 1
fi

echo "Migration safety check passed."
