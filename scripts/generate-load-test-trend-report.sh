#!/usr/bin/env bash
# Generates a markdown trend report from the structured load-test results
# persisted under tests/load/results/ (one JSON file per CI run, see
# docs/load-test-trends.md). Complements the static docs/load-test-results.md
# and the issue-49 CI regression gate with longer-horizon visibility into
# gradual, sub-threshold drift.
#
# Usage: ./scripts/generate-load-test-trend-report.sh
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

RESULTS_DIR="tests/load/results"
OUT_FILE="docs/load-test-trend-report.md"

{
  echo "# Load Test Trend Report"
  echo
  echo "Generated from JSON run records in \`$RESULTS_DIR\`. See"
  echo "\`docs/load-test-trends.md\` for the storage format and"
  echo "\`docs/load-test-results.md\` for the underlying scenarios."
  echo

  if [[ ! -d "$RESULTS_DIR" ]] || [[ -z "$(find "$RESULTS_DIR" -name '*.json' -print -quit 2>/dev/null)" ]]; then
    echo "_No run records found yet under \`$RESULTS_DIR\`._"
    exit 0
  fi

  echo "| Scenario | Timestamp | Git SHA | p50 (ms) | p99 (ms) | Throughput (req/s) | Error rate |"
  echo "|---|---|---|---|---|---|---|"

  for f in "$RESULTS_DIR"/*.json; do
    if command -v jq >/dev/null 2>&1; then
      jq -r '[.scenario, .timestamp, .git_sha, .p50_ms, .p99_ms, .throughput_rps, .error_rate] | "| " + (join(" | ")) + " |"' "$f"
    else
      echo "| (jq not installed — install jq to render $f) |"
    fi
  done
} > "$OUT_FILE"

echo "Wrote $OUT_FILE"
