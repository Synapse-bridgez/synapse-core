# Swap Engine Capacity Planning & Autoscaling (#1539)

Source of truth: [`src/swap/capacity_tiers.json`](../src/swap/capacity_tiers.json),
evaluated by `src/swap/capacity.rs`. Re-measure per-instance numbers with
`docker-compose.load.yml` + `benches/` after significant changes and update the JSON.

## Model
- Per instance: 40 orders/s sustained, 25 ms CPU/DB time per order across the full
  lifecycle (quote → execute → settle), 10 DB connections.
- Replicas = `ceil(peak / (per_instance_rate × target_utilization))`, floored at
  `min_replicas` (2, the failover floor) and capped by the Postgres budget.

| Tier   | Peak orders/s | Replicas | DB connections |
|--------|---------------|----------|----------------|
| small  | 50            | 2        | 20             |
| medium | 200           | 8        | 80             |
| large  | 400           | 16       | 160            |

Postgres `max_connections=200`, 40 reserved (admin, migrations, replicas) ⇒ budget 160
⇒ **max 16 replicas**. The `large` tier sits exactly at the budget; going beyond
requires PgBouncer or a bigger instance, not more replicas.

## Autoscaling policy
Extends `monitoring/autoscaling-policy.yaml` (HPA): target `pending_queue_depth` and
`db_pool_saturation_ratio` tied to the latency SLO, scale-up window 120 s (burst:
+100%/60 s), scale-down window 300 s at 20%/300 s (sustained). `minReplicas: 2`.
`maxReplicas` must never exceed `CapacityModel::max_replicas()` — scaling out when
the DB is saturated only amplifies the outage. Scale-down relies on graceful draining
(`terminationGracePeriodSeconds` ≥ longest order step) so no order is terminated mid-flight.

## Data tier
- Pool per instance = 10; total = replicas × 10 ≤ `max_connections − reserved`
  (asserted in `swap::capacity` tests).
- Add partitions (see `docs/partitioning.md`) when the hottest partition exceeds ~50M
  rows or its vacuum/index maintenance exceeds the maintenance window.
- Add a read replica when read-only queries (reporting, status polling) exceed ~40% of
  DB CPU while writes stay under 60%.

## Validation
`SWAP_LOAD_CONFIRM=yes scripts/swap-load-profile.sh` steps through every tier against
the load stack and fails if achieved rate deviates more than `SWAP_LOAD_TOLERANCE`
(default 15%). It refuses non-local targets.
