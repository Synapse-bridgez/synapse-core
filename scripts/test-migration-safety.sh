#!/usr/bin/env bash
# Regression test for scripts/check-migration-safety.sh's own coverage.
#
# The safety script's checks are pattern-based and easy to accidentally
# weaken (an over-eager exclusion, a regex typo) without anything else
# noticing, since there's no other signal that "this check still actually
# flags what it claims to flag". This runs it against fixtures known to be
# unsafe and safe, and fails if either result flips.
#
# Usage: ./scripts/test-migration-safety.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK_SCRIPT="$SCRIPT_DIR/check-migration-safety.sh"
FIXTURES_DIR="$SCRIPT_DIR/fixtures/migration-safety"
FAILURES=0

check_case() {
  local name="$1" dir="$2" expect="$3" # expect: "pass" or "fail"

  set +e
  output=$("$CHECK_SCRIPT" "$dir" 2>&1)
  status=$?
  set -e

  if [[ "$expect" == "fail" && "$status" -eq 0 ]]; then
    echo "FAIL: $name — expected check-migration-safety.sh to flag $dir, but it exited 0"
    echo "--- output ---"
    echo "$output"
    FAILURES=$((FAILURES + 1))
  elif [[ "$expect" == "pass" && "$status" -ne 0 ]]; then
    echo "FAIL: $name — expected check-migration-safety.sh to pass $dir, but it exited $status"
    echo "--- output ---"
    echo "$output"
    FAILURES=$((FAILURES + 1))
  else
    echo "OK: $name"
  fi
}

check_case "unsafe fixture (CREATE INDEX on pre-existing table) is flagged" \
  "$FIXTURES_DIR/unsafe" "fail"

check_case "safe fixture (CREATE INDEX on table created in same migration) passes" \
  "$FIXTURES_DIR/safe" "pass"

check_case "unsafe fixture (unguarded TYPE change on sensitive column) is flagged" \
  "$FIXTURES_DIR/unsafe-sensitive-type-change" "fail"

check_case "safe fixture (guarded TYPE change on sensitive column) passes" \
  "$FIXTURES_DIR/safe-guarded-sensitive-type-change" "pass"

if [[ "$FAILURES" -gt 0 ]]; then
  echo "$FAILURES fixture test(s) failed."
  exit 1
fi

echo "All migration-safety fixture tests passed."
