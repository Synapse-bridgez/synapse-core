#!/usr/bin/env bash
# Reset the sandbox to a clean state (drops sandbox DB volume, restarts stack).
# Refuses to run unless APP_ENV is explicitly sandbox - never touch production.
set -euo pipefail
[ "${APP_ENV:-}" = "sandbox" ] || { echo "refusing: set APP_ENV=sandbox" >&2; exit 1; }
f="$(dirname "$0")/../docker-compose.sandbox.yml"
docker compose -f "$f" down -v
docker compose -f "$f" up -d
echo "sandbox reset (non-production data only)"
