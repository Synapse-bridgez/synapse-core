#!/usr/bin/env bash
# Validates migration files and tests up→down→up idempotency.
# Usage: DATABASE_URL=<url> ./scripts/validate-migrations.sh
set -euo pipefail

MIGRATIONS_DIR="migrations"
ERRORS=0

echo "=== Migration Validation ==="

# 1. Check naming convention: must start with a 14-digit timestamp
echo "-- Checking naming convention..."
for f in "$MIGRATIONS_DIR"/*.sql; do
    base=$(basename "$f")
    if ! [[ "$base" =~ ^[0-9]{14}_ ]]; then
        echo "ERROR: Bad filename convention: $base"
        ERRORS=$((ERRORS + 1))
    fi
done

# 2. Check every .up/.sql migration has a matching .down.sql
echo "-- Checking down migrations exist..."
for f in "$MIGRATIONS_DIR"/*.sql; do
    base=$(basename "$f")
    # Skip files that are already .down.sql
    [[ "$base" == *.down.sql ]] && continue

    stem="${base%.sql}"
    # Handle both <name>.up.sql and <name>.sql conventions
    stem="${stem%.up}"

    down_file="$MIGRATIONS_DIR/${stem}.down.sql"
    if [[ ! -f "$down_file" ]]; then
        echo "ERROR: Missing down migration for: $base (expected $down_file)"
        ERRORS=$((ERRORS + 1))
    fi
done

if [[ $ERRORS -gt 0 ]]; then
    echo "FAIL: $ERRORS validation error(s) found."
    exit 1
fi
echo "OK: All migration files pass static checks."

# 3. Up → Down → Up idempotency (requires DATABASE_URL)
if [[ -z "${DATABASE_URL:-}" ]]; then
    echo "SKIP: DATABASE_URL not set, skipping idempotency test."
    exit 0
fi

echo "-- Running up→down→up idempotency test..."
sqlx migrate run
sqlx migrate revert --all
sqlx migrate run
echo "OK: Idempotency test passed."
