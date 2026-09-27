#!/usr/bin/env bash
# Generate a redacted, public-facing status report from health/readiness
# samples (src/health.rs, src/readiness.rs). Produces uptime percentage and
# an incident timeline suitable for a public status page.
#
# Out of scope: hosting a status page UI — this only produces the report data.
#
# Usage: scripts/public-status-report.sh <health-endpoint-url> [samples-file]
set -euo pipefail

ENDPOINT="${1:?usage: $0 <health-endpoint-url> [samples-file]}"
SAMPLES_FILE="${2:-status-samples.jsonl}"

# Sample the health endpoint and append a redacted record.
# Redaction: keep only overall status + timestamp, drop internal fields
# (dependency names, internal hostnames, stack traces) per issue 65's
# panic-recovery redaction principles.
timestamp="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
status="$(curl -fsS "${ENDPOINT}" 2>/dev/null | grep -o '"status"[[:space:]]*:[[:space:]]*"[a-zA-Z_]*"' | head -n1 | sed -E 's/.*"([a-zA-Z_]+)"$/\1/')"
status="${status:-unknown}"

printf '{"timestamp":"%s","status":"%s"}\n' "${timestamp}" "${status}" >> "${SAMPLES_FILE}"

echo "Recorded sample: ${timestamp} -> ${status} (${SAMPLES_FILE})"
echo
echo "# Public Status Report"
echo
total=$(wc -l < "${SAMPLES_FILE}" | tr -d ' ')
up=$(grep -c '"status":"ok"\|"status":"healthy"\|"status":"up"' "${SAMPLES_FILE}" || true)
if [ "${total}" -gt 0 ]; then
  pct=$(awk -v u="${up}" -v t="${total}" 'BEGIN { printf "%.2f", (u/t)*100 }')
else
  pct="0.00"
fi
echo "Uptime: ${pct}% (${up}/${total} samples healthy)"
echo
echo "## Incident Timeline (non-healthy samples)"
grep -v '"status":"ok"\|"status":"healthy"\|"status":"up"' "${SAMPLES_FILE}" || echo "_None recorded._"
