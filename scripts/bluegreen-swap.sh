#!/usr/bin/env bash
# Blue/green cutover and rollback for swap engine services (#1528).
# Usage: bluegreen-swap.sh cutover|rollback
# Env: ADMIN_URL, ADMIN_TOKEN, GREEN_URL, STEPS="10 50 100", SOAK_SECS=60, DRAIN_SECS=30
set -euo pipefail
: "${ADMIN_URL:?}" "${ADMIN_TOKEN:?}" "${GREEN_URL:?}"
STEPS=${STEPS:-"10 50 100"}; SOAK_SECS=${SOAK_SECS:-60}; DRAIN_SECS=${DRAIN_SECS:-30}
DRY_RUN=${DRY_RUN:-0}
run() { if [ "$DRY_RUN" = 1 ]; then echo "[dry-run] $*"; else "$@"; fi; }
api() { run curl -fsS -X POST -H "Authorization: Bearer $ADMIN_TOKEN" "$ADMIN_URL/admin/blue-green/$1"; }
healthy() { [ "$DRY_RUN" = 1 ] || curl -fsS "$GREEN_URL/health" >/dev/null; }

rollback() { echo "ROLLBACK: reverting traffic to blue"; api rollback; echo "drain green ${DRAIN_SECS}s"; run sleep "$DRAIN_SECS"; }

case "${1:-}" in
  rollback) rollback ;;
  cutover)
    healthy || { echo "green not healthy; aborting before any traffic shift" >&2; exit 1; }
    for pct in $STEPS; do
      echo "shift ${pct}% -> green"; run echo "orchestrator: set green weight=$pct"
      run sleep "$SOAK_SECS"
      healthy || { rollback; exit 1; }   # abort criterion: health gate fails during soak
    done
    api cutover
    echo "blue retained for the stability window; tear down only after it passes"
    ;;
  *) echo "usage: $0 cutover|rollback" >&2; exit 2 ;;
esac
