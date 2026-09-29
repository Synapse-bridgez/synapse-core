# Network Policy

The service policy is defined in `deploy/network-policy.yaml` and is default-deny for both ingress and egress. Synapse Core has explicit egress only to Postgres (`5432`), Redis (`6379`), Vault (`8200`), DNS (`53`), and HTTPS settlement APIs (`443`). Ingress is limited to the ingress controller on port `3000`.

The HTTPS rule intentionally excludes private RFC1918 ranges. Production overlays must replace the public `0.0.0.0/0` block with the settlement provider's published CIDRs. Run `scripts/lint-network-policy.sh` with `kubeconform` before applying changes.
