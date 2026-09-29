#!/usr/bin/env bash
set -euo pipefail

HEALTH_URL="${HEALTH_URL:-http://localhost:3000}"
HEALTH_WINDOW_SECONDS="${HEALTH_WINDOW_SECONDS:-300}"
HEALTH_FAILURE_THRESHOLD="${HEALTH_FAILURE_THRESHOLD:-2}"

failures=0
end=$((SECONDS + HEALTH_WINDOW_SECONDS))

while (( SECONDS < end )); do
  status_code=$(curl -fsS -o /tmp/health.json -w '%{http_code}' "${HEALTH_URL}/health" || true)
  ready_code=$(curl -fsS -o /tmp/ready.json -w '%{http_code}' "${HEALTH_URL}/ready" || true)

  if [[ "$status_code" != "200" ]]; then
    failures=$((failures + 1))
  fi

  if [[ "$ready_code" != "200" ]]; then
    failures=$((failures + 1))
  fi

  if [[ "$failures" -ge "$HEALTH_FAILURE_THRESHOLD" ]]; then
    echo "Post-deploy health checks failed; triggering rollback." >&2
    exit 1
  fi

  sleep 30
done

echo "Post-deploy health checks remained healthy for ${HEALTH_WINDOW_SECONDS}s."
