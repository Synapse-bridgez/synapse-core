#!/usr/bin/env bash
# Fail if any file in tests/ is never touched by any `cargo test` invocation
# in any workflow file.
#
# Context: this repo had (and, for files this script doesn't yet know to
# watch, may again have) integration test files that compile and pass when
# run locally but have zero CI execution history, because every workflow's
# `cargo test` invocation was either scoped to `--lib --bins` (excludes all
# of tests/) or to `-- --ignored` (only runs #[ignore]d tests, silently
# skipping any non-ignored test in a tests/*.rs file). A test file can look
# like real coverage and never actually run. See
# docs/ci-test-file-coverage.md for the full writeup and which files (if
# any) are deliberately excluded.
#
# This check is deliberately structural, not exhaustive: it does not know
# whether a `cargo test` invocation *passes*, only whether at least one
# invocation in the workflow files would *attempt to run* each test file's
# tests. A wildcard `cargo test` invocation (no `--lib`/`--bins`-only
# restriction and no narrowing `--test <name>`) counts as covering every
# file in tests/, present and future — which is what actually closes this
# gap, rather than a maintained list of `--test` flags that silently goes
# stale the next time someone adds a file.
#
# Usage: ./scripts/check-test-file-ci-coverage.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORKFLOWS_DIR="$REPO_ROOT/.github/workflows"
TESTS_DIR="$REPO_ROOT/tests"
ALLOWLIST_FILE="$REPO_ROOT/docs/ci-test-file-coverage.md"

# Every `cargo test ...` invocation line across every workflow file,
# collapsed to one line each so multi-line `if ! cargo test ... ; then`
# constructs still match. Written to a temp file (not a bash array via
# `mapfile`, which isn't available in bash 3.2 as shipped on macOS) so the
# rest of this script stays portable between local runs and CI.
INVOCATIONS_FILE="$(mktemp)"
trap 'rm -f "$INVOCATIONS_FILE"' EXIT
grep -hoE 'cargo test[^"'"'"'\n]*' "$WORKFLOWS_DIR"/*.yml > "$INVOCATIONS_FILE" || true

has_wildcard_invocation=false
while IFS= read -r inv; do
  [[ -z "$inv" ]] && continue
  if echo "$inv" | grep -qE -- '--lib\b|--bins\b|--test\b|--bin\b'; then
    continue
  fi
  has_wildcard_invocation=true
  break
done < "$INVOCATIONS_FILE"

if [[ "$has_wildcard_invocation" == true ]]; then
  echo "Found an unrestricted 'cargo test' invocation in .github/workflows/*.yml — it covers every file in tests/, present and future."
  echo "check-test-file-ci-coverage: passed."
  exit 0
fi

# No wildcard invocation exists (a regression from the state at the time
# this script was written) — fall back to requiring every tests/*.rs file
# be named explicitly via `--test <stem>` somewhere, or listed in the
# documented exclusion table.
echo "::warning::No unrestricted 'cargo test' invocation found in any workflow — falling back to per-file --test flag matching. This is more fragile; consider restoring a wildcard invocation. See docs/ci-test-file-coverage.md."

MISSING=()
for file in "$TESTS_DIR"/*.rs; do
  [[ -f "$file" ]] || continue
  stem="$(basename "$file" .rs)"

  referenced=false
  if grep -qE -- "--test[[:space:]]+$stem\b" "$INVOCATIONS_FILE"; then
    referenced=true
  fi

  if [[ "$referenced" == false ]] && [[ -f "$ALLOWLIST_FILE" ]] && grep -qF "$stem" "$ALLOWLIST_FILE"; then
    referenced=true
  fi

  if [[ "$referenced" == false ]]; then
    MISSING+=("$stem")
  fi
done

if [[ "${#MISSING[@]}" -gt 0 ]]; then
  echo "::error::The following tests/ files are not referenced by any 'cargo test' invocation in .github/workflows/*.yml, and are not listed in docs/ci-test-file-coverage.md as intentionally excluded:"
  for m in "${MISSING[@]}"; do
    echo "  - tests/${m}.rs"
  done
  exit 1
fi

echo "check-test-file-ci-coverage: passed (per-file matching)."
