#!/usr/bin/env bash
# Regression guard for Part C: the live /graphql handler must actually
# invoke the async_graphql schema it's supposedly wired to, not a hand-rolled
# stand-in that pattern-matches query text. This is a static, mechanical
# check (not a true runtime "is Schema::execute reachable from here"
# reflection, which Rust doesn't offer) — it exists specifically because
# this exact regression compiled cleanly and passed its own narrow tests
# last time, with no signal anywhere except actually reading the handler.
#
# Usage: ./scripts/check-graphql-handler-wired.sh
set -euo pipefail

HANDLER_FILE="${GRAPHQL_HANDLER_FILE:-src/handlers/graphql.rs}"

if [[ ! -f "$HANDLER_FILE" ]]; then
  echo "::error file=$HANDLER_FILE::GraphQL handler file not found"
  exit 1
fi

if ! grep -qE '\.graphql_schema\.execute\(' "$HANDLER_FILE"; then
  echo "::error file=$HANDLER_FILE::graphql_handler no longer calls state.graphql_schema.execute(...) — this is the exact regression Part C fixed (a hand-rolled stand-in silently replacing real schema execution). If this is intentional, update this check; if not, restore the real execute() call."
  exit 1
fi

if [[ -f "${HANDLER_FILE}.bak" ]]; then
  echo "::error file=${HANDLER_FILE}.bak::A .bak file exists next to the live handler — this is exactly the orphaned-backup pattern that caused the Part C regression. Merge its content in or delete it; do not leave both in the tree."
  exit 1
fi

echo "OK: $HANDLER_FILE calls graphql_schema.execute(...), no orphaned .bak file present"
