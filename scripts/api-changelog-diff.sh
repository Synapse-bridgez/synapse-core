#!/usr/bin/env bash
# Detect public API changes (GraphQL schema, REST routes) between two git refs
# and generate a structured changelog section classifying changes as
# breaking / non-breaking / additive.
#
# Usage: scripts/api-changelog-diff.sh <base-ref> <head-ref>
#
# Complements cliff.toml's changelog generation (issue: automated API change
# detection) and the compatibility matrix work (issue 92).
set -euo pipefail

BASE_REF="${1:-HEAD~1}"
HEAD_REF="${2:-HEAD}"
SCHEMA_PATH="src/graphql/schema.rs"

echo "# API Changelog: ${BASE_REF}..${HEAD_REF}"
echo
echo "## GraphQL Schema Changes"
echo
if git diff --quiet "${BASE_REF}" "${HEAD_REF}" -- "${SCHEMA_PATH}" 2>/dev/null; then
  echo "_No changes detected in ${SCHEMA_PATH}._"
else
  echo '```diff'
  git diff "${BASE_REF}" "${HEAD_REF}" -- "${SCHEMA_PATH}"
  echo '```'
  echo
  echo "> TODO: classify each hunk above as breaking / non-breaking / additive."
fi

echo
echo "## REST Route Changes"
echo
echo "_Route/handler signature diffing not yet implemented — see src/ for handler modules._"
