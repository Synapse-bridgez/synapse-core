# Swap Engine Operator Runbook (#1478)

Day-to-day operations, common failure modes, escalation paths, and how to inspect and intervene in the Phase 2 swap engine.

> **Flag-gated**: The swap engine is inactive unless the `swap_engine` feature flag is enabled for a tenant. All operations in this runbook apply only to tenants with the flag set.

## Table of Contents

1. [Dashboards to Check First](#dashboards-to-check-first)
2. [Common Alerts and Triage Steps](#common-alerts-and-triage-steps)
3. [Kill Switch and Circuit Breakers](#kill-switch-and-circuit-breakers)
4. [Inspecting Stuck Orders](#inspecting-stuck-orders)
5. [Escalation Contacts](#escalation-contacts)
6. [Routine Health Checks](#routine-health-checks)
7. [Scheduled Job Health](#scheduled-job-health)

---

## Dashboards to Check First

When something smells off with the swap engine, start here in order:

| Dashboard | What to look at | Threshold |
|---|---|---|
| **Swap Engine Overview** | `swap_orders_total` by status; `swap_engine_fill_rate` | Fill rate < 95% for 5 min → alert |
| **Queue Depth** | `swap_engine_queue_depth` gauge | > 500 pending → investigate |
| **Error Rate** | `swap_engine_errors_total` by error type | > 1% of requests for 2 min → alert |
| **Execution Latency** | `swap_execution_duration_seconds` p99 | p99 > 10 s → check LP availability |
| **Circuit Breaker** | `circuit_breaker_state{component="swap_engine"}` | `open` state → see §Kill Switch |
| **DB Pool** | `db_pool_usage_percent` | > 90% for 1 min → pool exhaustion |
| **Redis Hit Rate** | `redis_cache_hit_rate{cache="swap_quotes"}` | < 50% for 5 min → Redis issue |

Run the automated health check:

```bash
curl -s http://localhost:3000/health | jq .swap_engine
# Expected: { "status": "healthy", "circuit_breaker": "closed", "pending_orders": 0 }
```

---

## Common Alerts and Triage Steps

### SWAP_ENGINE_FILL_RATE_LOW

**Symptom:** Fill rate below 95% for more than 5 consecutive minutes.

**Triage:**
1. Check LP endpoint availability: `curl -s http://localhost:3000/health/liquidity`
2. Inspect circuit breaker state: `SELECT * FROM circuit_breaker_state WHERE component = 'swap_engine';`
3. Look for partial fills: `SELECT id, status, filled_amount, source_amount FROM swap_orders WHERE status = 'partially_filled' ORDER BY updated_at DESC LIMIT 20;`
4. If LP is down: see [Kill Switch and Circuit Breakers](#kill-switch-and-circuit-breakers).

---

### SWAP_ENGINE_STUCK_ORDERS

**Symptom:** Orders in `executing` or `reconciling` status for more than their configured timeout.

**Triage:**
```sql
-- Orders stuck in executing for > 10 minutes
SELECT id, status, created_at, updated_at,
       EXTRACT(EPOCH FROM (NOW() - updated_at)) / 60 AS stale_minutes
FROM swap_orders
WHERE status IN ('executing', 'reconciling')
  AND updated_at < NOW() - INTERVAL '10 minutes'
ORDER BY updated_at;
```

If rows are returned: see [Inspecting Stuck Orders](#inspecting-stuck-orders).

---

### SWAP_ENGINE_CIRCUIT_OPEN

**Symptom:** Circuit breaker for the swap engine or an LP is open.

**Triage:**
1. Check what opened the circuit: `SELECT * FROM circuit_breaker_state ORDER BY last_failure_at DESC LIMIT 5;`
2. Check the LP health: `curl -s http://localhost:3000/health/liquidity`
3. If LP has recovered, manually close the circuit (see §Kill Switch).

---

### SWAP_ENGINE_QUEUE_DEPTH_HIGH

**Symptom:** `swap_engine_queue_depth` > 500.

**Triage:**
1. Check if the swap worker is running: `systemctl status synapse-swap-worker`
2. Check for DB pool exhaustion: `SELECT count(*) FROM pg_stat_activity WHERE application_name LIKE '%synapse%';`
3. Check Redis: `redis-cli ping`

---

### SWAP_ENGINE_ERROR_RATE_HIGH

**Symptom:** Error rate > 1% of swap requests for 2 consecutive minutes.

**Triage:**
1. Check application logs for the error type:  
   `journalctl -u synapse-core --since "5 minutes ago" | grep -i "swap error"`
2. Correlate with LP errors, DB errors, or quota exhaustion.
3. If the error is `ERR_SWAP_004` (risk rejection) with `amount_limit_exceeded`, a tenant may be hitting their risk limit — no action required; the limit is intentional.

---

## Kill Switch and Circuit Breakers

### Disable swap engine for a tenant (kill switch)

```bash
# Disable via feature flag API
curl -X POST http://localhost:3000/admin/feature-flags \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"flag": "swap_engine", "tenant_id": "<tenant_id>", "enabled": false}'
```

After disabling: new transactions for that tenant will not be handed off to the swap engine. In-flight orders continue to completion; they are not cancelled.

### Process-wide kill switch (all tenants)

Set the environment variable and restart:

```bash
# In your deployment config / secrets manager:
SWAP_ENGINE_ENABLED=false

# Rolling restart (Kubernetes)
kubectl -n synapse rollout restart deployment/synapse-core
```

### Manually open/close a circuit breaker

```sql
-- Inspect current state
SELECT component, state, failure_count, last_failure_at, reset_at
FROM circuit_breaker_state;

-- Manually reset (close) a circuit
UPDATE circuit_breaker_state
SET state = 'closed', failure_count = 0, reset_at = NOW()
WHERE component = 'lp_endpoint';
```

> **Caution:** manually closing a circuit breaker that is open for a legitimate reason (LP still down) will cause immediate failures. Verify the LP is healthy before resetting.

---

## Inspecting Stuck Orders

Use these queries when an order is not progressing:

```sql
-- Order details and lifecycle events
SELECT o.id, o.status, o.source_asset, o.destination_asset,
       o.source_amount, o.filled_amount,
       o.created_at, o.updated_at,
       o.metadata->>'idempotency_key' AS idempotency_key
FROM swap_orders o
WHERE o.id = '<order_id>';

-- Full event log for the order
SELECT from_status, to_status, payload, created_at
FROM swap_events
WHERE order_id = '<order_id>'
ORDER BY created_at;

-- Active fills
SELECT id, source_amount, destination_amount, rate, external_ref, created_at
FROM swap_fills
WHERE order_id = '<order_id>'
ORDER BY created_at;
```

### Force-move a stuck order to `failed`

Only do this when the order is confirmed unrecoverable (LP has been contacted, no fill will arrive):

```sql
BEGIN;

-- Transition to failed
UPDATE swap_orders
SET status = 'failed', updated_at = NOW()
WHERE id = '<order_id>'
  AND status IN ('executing', 'reconciling');

-- Append lifecycle event
INSERT INTO swap_events (order_id, order_created_at, from_status, to_status, payload)
SELECT id, created_at, status, 'failed',
       jsonb_build_object('reason', 'operator_forced_failure', 'operator', 'your-name', 'ts', NOW())
FROM swap_orders
WHERE id = '<order_id>';

COMMIT;
```

> Log every manual intervention in the incident timeline.

### Manually retry a failed order

```sql
-- Return to 'pending' for the swap worker to retry
BEGIN;
UPDATE swap_orders
SET status = 'pending', updated_at = NOW()
WHERE id = '<order_id>'
  AND status = 'failed';
INSERT INTO swap_events (order_id, order_created_at, from_status, to_status, payload)
SELECT id, created_at, 'failed', 'pending',
       jsonb_build_object('reason', 'operator_retry', 'operator', 'your-name', 'ts', NOW())
FROM swap_orders WHERE id = '<order_id>';
COMMIT;
```

---

## Escalation Contacts

| Tier | Condition | Contact |
|---|---|---|
| Tier 1 | Auto-alert within SLO window | On-call via PagerDuty (#swap-oncall) |
| Tier 2 | 15 min unresolved / SLO breach | Team lead + #swap-incidents Slack |
| Tier 3 | Data corruption / LP contract dispute | Engineering director + LP support |
| LP issues | LP endpoint/settlement disputes | LP technical support (contact in 1Password) |

---

## Routine Health Checks

```bash
# Quick swap engine health
curl -s http://localhost:3000/health/swap | jq

# Pending order count
psql $DATABASE_URL -c "SELECT COUNT(*) FROM swap_orders WHERE status NOT IN ('filled','failed','cancelled');"

# Circuit breaker states
psql $DATABASE_URL -c "SELECT component, state, failure_count FROM circuit_breaker_state;"

# Last 10 swap events
psql $DATABASE_URL -c "SELECT order_id, from_status, to_status, created_at FROM swap_events ORDER BY created_at DESC LIMIT 10;"
```

---

## Scheduled Job Health

| Job | Schedule | Expected behavior | How to verify |
|---|---|---|---|
| Partition maintenance | Daily | Creates next-month partitions | `SELECT ensure_swap_partitions(6);` |
| Stuck order watchdog | Every 5 min | Quarantines stale executing orders | Check `transaction_dlq` table |
| Circuit breaker reset | Every 30 s | Probes LP and resets open circuits | `SELECT state FROM circuit_breaker_state;` |
| Backup | Daily 02:00 | Creates encrypted backup | Check `var/backup/` logs |

If a job has not run within 2× its expected interval, check `cron` logs and the `cron_jobs` table (if present), and restart the relevant service.
