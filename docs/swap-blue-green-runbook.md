# Swap Engine Blue/Green Deploy and Rollback (#1528)

Builds on the admin API in `docs/blue-green-deployment.md`; automation lives in
`scripts/bluegreen-swap.sh` (`DRY_RUN=1` for a rehearsal). Phased feature-flag
rollout (#1430), multi-region routing and migration authoring are out of scope.

## Cutover

1. Deploy green alongside blue (same schema, new image/config).
2. Gate: green `/health` and smoke tests pass, else abort (no traffic moved).
3. Shift traffic in steps (`STEPS`, default 10/50/100%), soaking `SOAK_SECS`
   and re-checking health at each step.
4. `POST /admin/blue-green/cutover`; keep blue running for the stability window,
   then tear it down.

## Rollback

Criteria: health gate fails, error rate/latency over SLO, or settlement
handoff failures during soak/stability window. The script auto-rolls back on a
failed gate; manual: `scripts/bluegreen-swap.sh rollback`, which reverts traffic
to blue and drains green for `DRAIN_SECS`. Revert config changes with the same
step by redeploying the previous config revision to the active slot.

## Schema compatibility (expand/contract)

During cutover both versions run against one schema: ship additive migrations
first (new nullable columns/tables), deploy, and only drop/rename in a later
release once no running version reads the old shape. Never ship a migration the
previous version cannot run against - this is what keeps rollback safe.

## Draining and long-running swaps

Stop routing new work to the old slot, then wait `DRAIN_SECS` (set it above the
longest expected swap/settlement handoff) before stopping it. Handoffs are
idempotent (`SwapHandoff.idempotency_key`), so a re-delivered handoff after
rollback is safe. Re-running the script is idempotent.
