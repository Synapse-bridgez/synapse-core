# Swap Engine Phased Rollout

The swap engine is a money-moving path. It rolls out in stages so a defect has
a bounded blast radius and can be stopped quickly. A tenant can start a new
hand-off only when **both** keys pass:

1. **Env gate** (`src/config.rs`, `Config::swap_engine_allows_tenant`):
   - `SWAP_ENGINE_ENABLED` must be `true` (process-wide kill switch), and
   - the tenant id must appear in `SWAP_ENGINE_TENANT_ALLOWLIST` (`*` admits all).
2. **Tenant flag** (`swap_engine` row in `feature_flags`, checked by
   `swap::swap_enabled_for`): must be enabled, with `rollout_percentage` applied
   by a stable per-tenant hash.

The env gate is checked first. When it is closed, no DB lookup happens and no
new hand-off starts, regardless of the flag.

## Environment profiles

| Environment | `SWAP_ENGINE_ENABLED` | `SWAP_ENGINE_TENANT_ALLOWLIST` | Source |
|---|---|---|---|
| Development | `true` | `*` | `.env.development` |
| Production | `false` (dark) | empty | `.env.production` |
| Failover / standby | `false` | empty | `.env.example.failover` |

Production ships dark. Set the variables through the secrets manager or
platform config, not by editing the committed profile.

## Rollout sequence

| Stage | Env gate | `swap_engine` flag | Exit criteria |
|---|---|---|---|
| 0. Dark launch | `SWAP_ENGINE_ENABLED=true`, allowlist empty | enabled | Engine deployed, no tenant admitted, no hand-offs |
| 1. Internal tenant | allowlist = internal tenant id | enabled, 100% | Reconciliation clean for a full settlement cycle |
| 2. Limited partners | allowlist = partner ids | enabled, `rollout_percentage` 10 → 50 | No Sev-2+ incidents, error rate within budget |
| 3. General availability | allowlist = `*` | enabled, 100% | Partner stages stable for one full cycle |

Move one stage at a time. Stop and roll back if an exit criterion fails.

### Stage 0: register the flag (disabled)

```sql
INSERT INTO feature_flags (name, enabled, description)
VALUES ('swap_engine', false, 'Phase 2 swap engine hand-off')
ON CONFLICT (name) DO NOTHING;
```

A missing row is treated as disabled, so this step is for visibility, not safety.

### Enabling a stage

```sql
-- Stage 2: partner ramp
UPDATE feature_flags SET rollout_percentage = 10 WHERE name = 'swap_engine';
```

Env changes require a restart of the service. Flag changes apply without a
redeploy.

## Kill switch

Use the fastest lever that stops the problem:

1. **Stop new orders for everyone (runtime, no restart):**

   ```sql
   UPDATE feature_flags SET enabled = false WHERE name = 'swap_engine';
   ```

   The admin flag endpoint is not routed yet (`/admin/flags` returns 501), so
   use SQL. Record the change in your incident log, since this statement does
   not write to `feature_flag_audit_logs`.

2. **Stop new orders at the process level (requires restart):** set
   `SWAP_ENGINE_ENABLED=false`. Use this when the flag cannot be changed, or to
   block a standby region.

Both levers block **new** hand-offs only. Orders already past the gate are not
cancelled.

## In-flight orders

Killing the engine must not strand orders in `Executing` or `SettlementPending`.
The reconciler must keep running after the kill switch is flipped and drive
those orders to a terminal state. Do not stop the reconciler to "stop the
engine". Check that the reconciler is still running with the same health checks
you use for the processor.

## Stuck-order recovery

1. Confirm the kill switch is off (step 1 or 2 above) so no new orders enter.
2. List orders stuck in a non-terminal state for longer than the expected
   settlement window. Compare against the state machine in
   `docs/state-machine.md`.
3. Let the reconciler retry first. Do not edit order state by hand.
4. If an order is still stuck after the reconciler's retry budget, escalate with
   the transaction id, tenant id, last state, and last `updated_at`. Manual
   transitions go through the normal admin path and must be audited.

## Rollback

- **Partial rollback (one tenant or partner):** remove the id from
  `SWAP_ENGINE_TENANT_ALLOWLIST` and restart, or lower `rollout_percentage`.
- **Full rollback:** disable the `swap_engine` flag (kill switch step 1), then set
  `SWAP_ENGINE_ENABLED=false` at the next restart.
- **Code rollback:** redeploy the previous release. The new config keys default
  to off, so rolling back the code leaves the swap path closed.

## Notes

- `SWAP_ENGINE_ENABLED` and `SWAP_ENGINE_TENANT_ALLOWLIST` are read at startup
  only. Changing them needs a restart.
- The allowlist is a comma-separated list of tenant id strings, matched exactly.
  An unknown value of `SWAP_ENGINE_ENABLED` fails startup instead of silently
  enabling or disabling the engine.
