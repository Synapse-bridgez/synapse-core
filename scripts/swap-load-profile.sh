#!/usr/bin/env bash
# Repeatable stepped load profile for the swap capacity tiers (#1539).
# Reads tiers from src/swap/capacity_tiers.json and drives each tier's peak
# rate with `hey` against the load stack (docker-compose.load.yml).
# Refuses to run unless SWAP_LOAD_CONFIRM=yes and the target is a local/load host.
set -euo pipefail
cd "$(dirname "$0")/.."
TARGET="${SWAP_LOAD_TARGET:-http://localhost:8080}"
TOL="${SWAP_LOAD_TOLERANCE:-0.15}"
DURATION="${SWAP_LOAD_DURATION:-30s}"

[[ "${SWAP_LOAD_CONFIRM:-}" == "yes" ]] || { echo "set SWAP_LOAD_CONFIRM=yes to run"; exit 2; }
case "$TARGET" in
  http://localhost*|http://127.0.0.1*|http://synapse-load*) ;;
  *) echo "refusing non-load target: $TARGET" >&2; exit 2 ;;
esac
command -v hey >/dev/null || { echo "hey is required" >&2; exit 2; }

jq -c '.tiers[]' src/swap/capacity_tiers.json | while read -r tier; do
  name=$(jq -r .name <<<"$tier"); rate=$(jq -r .peak_orders_per_sec <<<"$tier")
  echo "== tier $name @ ${rate} rps for $DURATION"
  out=$(hey -z "$DURATION" -q "$rate" -c 1 "$TARGET/health" | awk '/Requests\/sec/ {print $2}')
  python3 - "$name" "$rate" "$out" "$TOL" <<'PY'
import sys
n, want, got, tol = sys.argv[1], float(sys.argv[2]), float(sys.argv[3]), float(sys.argv[4])
ok = abs(got - want) / want <= tol
print(f"{n}: target {want} rps, achieved {got} rps -> {'OK' if ok else 'OUT OF TOLERANCE'}")
sys.exit(0 if ok else 1)
PY
done
