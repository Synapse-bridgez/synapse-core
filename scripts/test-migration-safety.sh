#!/usr/bin/env bash
# test-migration-safety.sh
# Runs the migration safety linter against fixtures and the real migration history.
# Each fixture directory is either "unsafe-*" (must fail) or "safe-*" (must pass).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECKER="${SCRIPT_DIR}/check-migration-safety.sh"
FIXTURES_DIR="${SCRIPT_DIR}/fixtures/migration-safety"

PASS=0
FAIL=0

run_expect() {
  local label="$1"
  local expect="$2"   # "pass" or "fail"
  local file="$3"

  if "$CHECKER" "$file" > /dev/null 2>&1; then
    actual="pass"
  else
    actual="fail"
  fi

  if [[ "$actual" == "$expect" ]]; then
    echo "  [OK]   $label — expected $expect, got $actual"
    PASS=$((PASS + 1))
  else
    echo "  [FAIL] $label — expected $expect, got $actual"
    FAIL=$((FAIL + 1))
  fi
}

echo "=== Fixture Tests ==="

for dir in "${FIXTURES_DIR}"/*/; do
  name="$(basename "$dir")"
  sql="${dir}migration.sql"
  [[ -f "$sql" ]] || continue

  if [[ "$name" == unsafe-* ]]; then
    run_expect "$name" "fail" "$sql"
  elif [[ "$name" == safe-* ]]; then
    run_expect "$name" "pass" "$sql"
  else
    echo "  [SKIP] $name — not prefixed safe-/unsafe-, skipping"
  fi
done

echo ""
echo "=== Regression Check Against migrations/ ==="
if "$CHECKER" > /dev/null 2>&1; then
  echo "  [OK]   All existing migrations pass the linter."
  PASS=$((PASS + 1))
else
  echo "  [FAIL] Existing migrations triggered safety violations — review allowlist."
  FAIL=$((FAIL + 1))
fi

echo ""
echo "Results: $PASS passed, $FAIL failed."
[[ $FAIL -eq 0 ]] && exit 0 || exit 1
