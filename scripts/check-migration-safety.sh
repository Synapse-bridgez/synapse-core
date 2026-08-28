#!/usr/bin/env bash
# check-migration-safety.sh
# Heuristic/pattern-based migration safety linter.
# Usage: ./scripts/check-migration-safety.sh [file-or-dir...]
#   Default: checks all migrations/ directory.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ALLOWLIST="${SCRIPT_DIR}/migration-safety-known-index-locks.txt"

# Collect files to check
if [[ $# -gt 0 ]]; then
  FILES=("$@")
else
  mapfile -t FILES < <(find migrations -name "*.sql" ! -name "*.down.sql" | sort)
fi

# Load allowlist entries (trimmed, non-comment lines)
declare -A ALLOWED=()
if [[ -f "$ALLOWLIST" ]]; then
  while IFS= read -r line; do
    line="${line%%#*}"   # strip inline comments
    line="${line//[[:space:]]/}"
    [[ -z "$line" ]] && continue
    ALLOWED["$line"]=1
  done < "$ALLOWLIST"
fi

ERRORS=0

check_file() {
  local file="$1"
  local basename
  basename="$(basename "$file")"

  # Helper: emit error unless file is allowlisted
  flag() {
    local rule="$1"
    local msg="$2"
    local key="${basename}:${rule}"
    if [[ -n "${ALLOWED[$key]+_}" ]]; then
      echo "  [ALLOWED]  $file — $rule (via allowlist)"
    else
      echo "  [ERROR]    $file — $rule: $msg"
      ERRORS=$((ERRORS + 1))
    fi
  }

  local content
  content="$(cat "$file")"
  # Strip single-line SQL comments (-- ...) before pattern matching to avoid false positives
  local stripped
  stripped="$(echo "$content" | sed 's/--[^\n]*//g')"
  # normalise to lower-case for pattern matching
  local lower
  lower="$(echo "$stripped" | tr '[:upper:]' '[:lower:]')"

  # ── Rule 1: sensitive type changes ─────────────────────────────────────────
  # ALTER COLUMN … TYPE changes that are known lossy/unsafe without a USING guard
  # Unsafe: plain ALTER COLUMN x TYPE (no USING clause on the same logical line)
  # Safe example: ALTER COLUMN x TYPE newtype USING x::newtype
  while IFS= read -r line; do
    local ll="${line,,}"  # lower
    if echo "$ll" | grep -qP 'alter\s+column\s+\S+\s+type\s+'; then
      if ! echo "$ll" | grep -qP '\busing\b'; then
        flag "sensitive-type-change" "ALTER COLUMN … TYPE without USING clause: $line"
      fi
    fi
  done < <(echo "$stripped" | grep -i 'alter column' || true)

  # ── Rule 2: NOT NULL column added without DEFAULT ──────────────────────────
  # ADD COLUMN x <type> NOT NULL  — no DEFAULT means table rewrite / lock on hot tables
  # Safe: ADD COLUMN x <type> NOT NULL DEFAULT <val>
  while IFS= read -r line; do
    local ll="${line,,}"
    if echo "$ll" | grep -qP 'add\s+column(\s+if\s+not\s+exists)?\s+\S+\s+\S+.*not\s+null'; then
      if ! echo "$ll" | grep -qP '\bdefault\b'; then
        flag "not-null-without-default" "ADD COLUMN NOT NULL without DEFAULT: $line"
      fi
    fi
  done < <(echo "$stripped" | grep -i 'add column' || true)

  # ── Rule 3: non-concurrent index creation ──────────────────────────────────
  # CREATE INDEX (without CONCURRENTLY) takes a write lock on the table.
  # CREATE INDEX IF NOT EXISTS also locks without CONCURRENTLY.
  # Skip: lines already using CONCURRENTLY
  while IFS= read -r line; do
    local ll="${line,,}"
    # only fire on CREATE INDEX lines
    if echo "$ll" | grep -qP '^\s*create\s+(unique\s+)?index\b'; then
      if ! echo "$ll" | grep -qP '\bconcurrently\b'; then
        flag "non-concurrent-index" "CREATE INDEX without CONCURRENTLY (takes lock): $line"
      fi
    fi
  done < <(echo "$stripped" | grep -i 'create.*index' || true)

  # ── Rule 4: non-concurrent constraint (NOT VALID guard missing) ────────────
  # ADD CONSTRAINT without NOT VALID + VALIDATE CONSTRAINT pattern holds a full lock.
  while IFS= read -r line; do
    local ll="${line,,}"
    if echo "$ll" | grep -qP 'add\s+constraint\b'; then
      if ! echo "$ll" | grep -qP '\bnot\s+valid\b'; then
        flag "constraint-without-not-valid" "ADD CONSTRAINT without NOT VALID (full lock): $line"
      fi
    fi
  done < <(echo "$stripped" | grep -i 'add constraint' || true)

  # ── Rule 5: column rename ──────────────────────────────────────────────────
  # RENAME COLUMN can break app code that references the old name.
  while IFS= read -r line; do
    local ll="${line,,}"
    if echo "$ll" | grep -qP '\brename\s+column\b'; then
      flag "rename-column" "RENAME COLUMN detected — ensure no app code references old name: $line"
    fi
  done < <(echo "$stripped" | grep -i 'rename column' || true)

  # ── Rule 6: table rename ───────────────────────────────────────────────────
  # RENAME TABLE / ALTER TABLE … RENAME TO can break app code.
  while IFS= read -r line; do
    local ll="${line,,}"
    if echo "$ll" | grep -qP '\brename\s+to\b'; then
      flag "rename-table" "RENAME TO detected — ensure no app code references old name: $line"
    fi
  done < <(echo "$stripped" | grep -i 'rename to' || true)
}

echo "=== Migration Safety Check ==="
for f in "${FILES[@]}"; do
  check_file "$f"
done

echo ""
if [[ $ERRORS -eq 0 ]]; then
  echo "✓ All checks passed (${#FILES[@]} file(s) checked)."
else
  echo "✗ $ERRORS unsafe pattern(s) found. Add entries to scripts/migration-safety-known-index-locks.txt to allowlist justified exceptions."
  exit 1
fi
