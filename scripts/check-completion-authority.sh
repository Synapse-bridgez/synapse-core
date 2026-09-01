#!/usr/bin/env bash
# Enforcement guard for docs/adr/005-transaction-completion-pipeline-authority.md
# and docs/adr-005-completion-authority-audit.md.
#
# ADR-005 designates processor.rs::process_batch as the sole authoritative
# path that writes a transaction's status to `completed`. This script fails
# if any other src/*.rs file writes that status directly, so a bypass (e.g.
# from settlement netting, dispute resolution, bulk status updates, or
# swap/bridge readiness code) can't be reintroduced silently.
#
# Usage: ./scripts/check-completion-authority.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

AUTHORITATIVE_FILE="src/processor.rs"
PATTERN="status[[:space:]]*=[[:space:]]*'completed'|status:[[:space:]]*(Transaction)?Status::Completed"

VIOLATIONS=()

while IFS= read -r -d '' file; do
  [[ "$file" == "$AUTHORITATIVE_FILE" ]] && continue
  if grep -qE "$PATTERN" "$file"; then
    VIOLATIONS+=("$file")
  fi
done < <(find src -name '*.rs' -print0)

if [[ ${#VIOLATIONS[@]} -gt 0 ]]; then
  echo "ERROR: found direct transaction-completion writes outside the ADR-005 authority ($AUTHORITATIVE_FILE):"
  for v in "${VIOLATIONS[@]}"; do
    echo "  - $v"
  done
  echo
  echo "Route completion through $AUTHORITATIVE_FILE instead, per docs/adr/005-transaction-completion-pipeline-authority.md."
  exit 1
fi

echo "OK: no completion-authority bypass found outside $AUTHORITATIVE_FILE"
