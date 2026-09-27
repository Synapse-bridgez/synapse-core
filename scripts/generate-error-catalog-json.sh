#!/usr/bin/env bash
# Generates docs/error-catalog.json from docs/error-catalog.md's
# `| Code | HTTP Status | Description |` tables, so SDKs (issue 88's Rust
# sync check, and future Python/TypeScript SDKs from issues 81/82) can
# consume a single machine-readable artifact instead of hand-parsing
# markdown or maintaining an independent copy of the mapping.
#
# docs/error-catalog.md remains the source of truth; this script only
# projects it into JSON. Re-run after editing the markdown catalog so the
# two cannot drift.
#
# Usage: ./scripts/generate-error-catalog-json.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

SRC="docs/error-catalog.md"
OUT="docs/error-catalog.json"

entries=()
while IFS='|' read -r _ code status description _; do
  code="$(echo "$code" | xargs)"
  status="$(echo "$status" | xargs)"
  description="$(echo "$description" | xargs | sed 's/"/\\"/g')"

  [[ "$code" =~ ^ERR_ ]] || continue

  entries+=("    {\"code\": \"$code\", \"http_status\": $status, \"graphql_surfacing\": \"extensions.code\", \"description\": \"$description\"}")
done < <(grep -E '^\| ERR_' "$SRC")

{
  echo "{"
  echo "  \"\$schema_note\": \"Generated from $SRC — do not edit by hand.\","
  echo "  \"error_response_format\": {"
  echo "    \"rest\": {\"body_fields\": [\"error\", \"code\", \"status\"]},"
  echo "    \"graphql\": {\"status\": 200, \"code_field\": \"errors[].extensions.code\"}"
  echo "  },"
  echo "  \"errors\": ["
  last=$((${#entries[@]} - 1))
  for i in "${!entries[@]}"; do
    if [[ "$i" -eq "$last" ]]; then
      echo "${entries[$i]}"
    else
      echo "${entries[$i]},"
    fi
  done
  echo "  ]"
  echo "}"
} > "$OUT"

echo "Wrote $OUT ($(printf '%s\n' "${entries[@]}" | wc -l) error codes)"
