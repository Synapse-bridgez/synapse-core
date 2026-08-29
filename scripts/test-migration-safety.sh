#!/usr/bin/env bash
# Regression test for scripts/check-migration-safety.sh coverage.
# Usage: ./scripts/test-migration-safety.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK_SCRIPT="$SCRIPT_DIR/check-migration-safety.sh"
FIXTURES_DIR="$SCRIPT_DIR/fixtures/migration-safety"
FAILURES=0

check_case() {
  local name="$1" dir="$2" expect="$3"

  set +e
  output=$("$CHECK_SCRIPT" "$dir" 2>&1)
  status=$?
  set -e

  if [[ "$expect" == "fail" && "$status" -eq 0 ]]; then
    echo "FAIL: $name -- expected check to flag $dir, but it exited 0"
    echo "--- output ---"
    echo "$output"
    FAILURES=$((FAILURES + 1))
  elif [[ "$expect" == "pass" && "$status" -ne 0 ]]; then
    echo "FAIL: $name -- expected check to pass $dir, but it exited $status"
    echo "--- output ---"
    echo "$output"
    FAILURES=$((FAILURES + 1))
  else
    echo "OK: $name"
  fi
}

# Existing fixtures
check_case "unsafe fixture (CREATE INDEX on pre-existing table) is flagged" \
  "$FIXTURES_DIR/unsafe" "fail"

check_case "safe fixture (CREATE INDEX on table created in same migration) passes" \
  "$FIXTURES_DIR/safe" "pass"

check_case "unsafe fixture (unguarded TYPE change on sensitive column) is flagged" \
  "$FIXTURES_DIR/unsafe-sensitive-type-change" "fail"

check_case "safe fixture (guarded TYPE change on sensitive column) passes" \
  "$FIXTURES_DIR/safe-guarded-sensitive-type-change" "pass"

check_case "unsafe fixture (ADD COLUMN NOT NULL without DEFAULT) is flagged" \
  "$FIXTURES_DIR/unsafe-not-null-no-default" "fail"

check_case "safe fixture (ADD COLUMN NOT NULL with DEFAULT) passes" \
  "$FIXTURES_DIR/safe-not-null-with-default" "pass"

check_case "unsafe fixture (RENAME COLUMN) is flagged" \
  "$FIXTURES_DIR/unsafe-rename-column" "fail"

check_case "safe fixture (expand/contract instead of RENAME COLUMN) passes" \
  "$FIXTURES_DIR/safe-rename-column-expand-contract" "pass"

check_case "unsafe fixture (RENAME TABLE) is flagged" \
  "$FIXTURES_DIR/unsafe-rename-table" "fail"

check_case "safe fixture (view shim instead of RENAME TABLE) passes" \
  "$FIXTURES_DIR/safe-rename-table-view-shim" "pass"

if [[ "$FAILURES" -gt 0 ]]; then
  echo "$FAILURES fixture test(s) failed."
  exit 1
fi

echo "All migration-safety fixture tests passed."
