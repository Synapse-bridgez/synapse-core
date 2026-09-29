#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "Usage: $0 --confirm-failover <target-region>"
  echo "Required: DNS_CUTOVER_COMMAND, DB_PROMOTION_COMMAND, SERVICE_RESTART_COMMAND, HEALTHCHECK_URL"
}

if [[ "${1:-}" != "--confirm-failover" || -z "${2:-}" ]]; then
  usage
  exit 2
fi

target_region="$2"
for required in DNS_CUTOVER_COMMAND DB_PROMOTION_COMMAND SERVICE_RESTART_COMMAND HEALTHCHECK_URL; do
  [[ -n "${!required:-}" ]] || { echo "Missing ${required}" >&2; exit 2; }
done

log_dir="${DR_LOG_DIR:-./var/dr-failover}"
mkdir -p "${log_dir}"
log_file="${log_dir}/failover-${target_region}-$(date -u +%Y%m%dT%H%M%SZ).log"
exec > >(tee -a "${log_file}") 2>&1

step() {
  local name="$1"
  shift
  echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] START ${name}"
  "$@"
  echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] DONE ${name}"
}

cat <<EOF
DISASTER RECOVERY FAILOVER
Target region: ${target_region}
This command changes DNS, promotes the replica, and restarts service traffic.
Execution log: ${log_file}
EOF
read -r -p "Type FAILOVER-${target_region} to continue: " confirmation
[[ "${confirmation}" == "FAILOVER-${target_region}" ]] || { echo "Confirmation did not match; no changes made."; exit 3; }

step "DNS/traffic cutover" bash -c "${DNS_CUTOVER_COMMAND}"
step "database replica promotion" bash -c "${DB_PROMOTION_COMMAND}"
step "service restart against promoted primary" bash -c "${SERVICE_RESTART_COMMAND}"

cat <<EOF
POST-FAILOVER VERIFICATION CHECKLIST
[ ] DNS resolves to ${target_region}
[ ] Promoted database accepts read/write traffic
[ ] Service readiness is healthy
[ ] Webhook ingestion succeeds
[ ] Settlement network connectivity is healthy
[ ] Error rate and queue depth are within normal range
EOF

step "readiness verification" curl --fail --silent --show-error "${HEALTHCHECK_URL}/ready"
step "health verification" curl --fail --silent --show-error "${HEALTHCHECK_URL}/health"
echo "Failover completed. Review and record every checklist item in the incident timeline."
