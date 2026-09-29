#!/usr/bin/env bash
set -euo pipefail

policy_file="${1:-deploy/network-policy.yaml}"
command -v kubeconform >/dev/null 2>&1 || {
  echo "kubeconform is required to validate ${policy_file}" >&2
  exit 1
}

kubeconform -strict -schema-location default "${policy_file}"

grep -q 'name: synapse-core-default-deny' "${policy_file}"
grep -q 'policyTypes:' "${policy_file}"
grep -q 'port: 5432' "${policy_file}"
grep -q 'port: 6379' "${policy_file}"
grep -q 'port: 8200' "${policy_file}"
grep -q 'port: 443' "${policy_file}"
echo "network policy validation passed"
