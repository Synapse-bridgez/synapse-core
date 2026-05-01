#!/usr/bin/env bash
# Check SQL migration files for potentially unsafe operations.
# Usage: ./scripts/check-migration-safety.sh <migrations-dir>
set -euo pipefail

MIGRATIONS_DIR="${1:?Usage: $0 <migrations-dir>}"
ERRORS=0

# Patterns considered unsafe in up migrations (not .down.sql files)
UNSAFE_PATTERNS=(
  "DROP TABLE"
  "DROP COLUMN"
  "TRUNCATE"
  "ALTER COLUMN .* TYPE"
  "ALTER COLUMN .* SET NOT NULL"
)

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
done

if [[ $ERRORS -gt 0 ]]; then
  echo "Found $ERRORS potentially unsafe migration operation(s). Review before merging."
  exit 1
fi

echo "Migration safety check passed."
