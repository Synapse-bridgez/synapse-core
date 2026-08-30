# Synapse Core Operational Runbook

## Table of Contents

1. [Overview](#overview)
2. [System Health Checks](#system-health-checks)
3. [Database Operations](#database-operations)
4. [Monitoring & Alerting](#monitoring--alerting)
5. [Incident Response](#incident-response)
6. [Maintenance Tasks](#maintenance-tasks)
7. [Troubleshooting](#troubleshooting)
8. [Deployment Procedures](#deployment-procedures)
9. [Security Operations](#security-operations)
10. [Backup & Recovery](#backup--recovery)

---

## Overview

This runbook provides step-by-step procedures for operating and maintaining the Synapse Core fiat gateway callback processor. It covers routine operations, incident response, and disaster recovery scenarios.

**System Components:**
- Synapse Core Application (Rust/Axum)
- PostgreSQL 14+ (Primary & Optional Replica)
- Redis (Idempotency & Caching)
- Stellar Horizon API (External)

**Key Documentation:**
- [Architecture](architecture.md)
- [Setup Guide](setup.md)
- [Database Failover](database_failover.md)
- [Disaster Recovery](disaster-recovery.md)

---

## System Health Checks

### Quick Health Check

```bash
# Check application health
curl http://localhost:3000/health | jq

# Expected response:
# {
#   "status": "healthy",
#   "version": "0.1.0",
#   "db_primary": "connected",
#   "db_replica": "connected",
#   "db_pool": {
#     "active_connections": 2,
#     "idle_connections": 3,
#     "max_connections": 5,
#     "usage_percent": 40.0
#   }
# }
```

### Component Health Checks

#### 1. Application Status
```bash
# Check if service is running
systemctl status synapse-core

# Or with Docker
docker ps | grep synapse-core

# Check application logs
journalctl -u synapse-core -f
# Or
docker logs -f synapse-core
```

#### 2. Database Connectivity
```bash
# Test primary database
psql $DATABASE_URL -c "SELECT 1;"

# Test replica database (if configured)
psql $DATABASE_REPLICA_URL -c "SELECT 1;"

# Check database connections
psql $DATABASE_URL -c "SELECT count(*) FROM pg_stat_activity WHERE datname = 'synapse';"
```

#### 3. Redis Connectivity
```bash
# Test Redis connection
redis-cli -u $REDIS_URL ping
# Expected: PONG

# Check Redis memory usage
redis-cli -u $REDIS_URL INFO memory | grep used_memory_human
```

#### 4. Stellar Horizon API
```bash
# Test Horizon connectivity
curl https://horizon-testnet.stellar.org/

# Check circuit breaker state (via logs)
grep "circuit breaker" /var/log/synapse-core/app.log
```

---

## Database Operations

### Connection Pool Monitoring

#### Check Pool Status
```bash
# Via health endpoint
curl http://localhost:3000/health | jq '.db_pool'

# Check for high usage warnings in logs
grep "Database connection pool usage high" /var/log/synapse-core/app.log
```

#### Pool Exhaustion Response
If pool usage is consistently ≥80%:

1. **Immediate**: Check for connection leaks
   ```bash
   # Check long-running queries
   psql $DATABASE_URL -c "
   SELECT pid, now() - query_start AS duration, query 
   FROM pg_stat_activity 
   WHERE state = 'active' AND now() - query_start > interval '30 seconds'
   ORDER BY duration DESC;"
   ```

2. **Short-term**: Increase pool size
   ```rust
   // In src/db/mod.rs or src/db/pool_manager.rs
   PgPoolOptions::new()
       .max_connections(10)  // Increase from 5
       .connect(&config.database_url)
       .await
   ```

3. **Long-term**: Scale horizontally (add more app instances)

### Partition Management

#### Check Partition Status
```sql
-- List all partitions with sizes
SELECT 
    c.relname AS partition_name,
    pg_size_pretty(pg_total_relation_size(c.oid)) AS size,
    pg_get_expr(c.relpartbound, c.oid) AS partition_bound
FROM pg_class c
JOIN pg_inherits i ON c.oid = i.inhrelid
JOIN pg_class p ON i.inhparent = p.oid
WHERE p.relname = 'transactions'
ORDER BY c.relname;
```

#### Manual Partition Operations
```sql
-- Create next month's partition
SELECT create_monthly_partition();

-- Detach partitions older than 12 months
SELECT detach_old_partitions(12);

-- Run full maintenance (create + detach)
SELECT maintain_partitions();
```

#### Partition Maintenance Schedule
- **Automatic**: Runs every 24 hours via background task
- **Manual**: Run before month-end if automatic task fails
- **Monitoring**: Check logs for partition creation/detachment events

```bash
# Check partition maintenance logs
grep "partition" /var/log/synapse-core/app.log | tail -20
```

### Database Failover

#### Check Replication Status
```sql
-- On primary database
SELECT * FROM pg_stat_replication;

-- Check replication lag
SELECT 
    client_addr,
    state,
    sync_state,
    pg_wal_lsn_diff(pg_current_wal_lsn(), replay_lsn) AS lag_bytes
FROM pg_stat_replication;
```

#### Failover to Replica
See [Database Failover](database_failover.md) for detailed procedures.

**Quick Steps:**
1. Verify replica is healthy
2. Stop application traffic
3. Promote replica to primary
4. Update `DATABASE_URL` in application config
5. Restart application
6. Resume traffic

**Estimated Time:** 15-20 minutes

---

## Monitoring & Alerting

### Key Metrics to Monitor

#### Application Metrics
- **Request Rate**: Requests per second to `/callback/transaction`
- **Error Rate**: 4xx and 5xx response rates
- **Response Time**: P50, P95, P99 latencies
- **Circuit Breaker State**: Open/Closed status for Horizon client

#### Database Metrics
- **Connection Pool Usage**: Active/idle connections, usage percentage
- **Query Performance**: Slow query count, average query time
- **Replication Lag**: Bytes behind primary (if using replica)
- **Partition Count**: Number of active partitions

#### System Metrics
- **CPU Usage**: Application and database CPU
- **Memory Usage**: Application heap, database cache
- **Disk I/O**: Database read/write IOPS
- **Network**: Inbound/outbound traffic

### Alert Thresholds

| Alert | Threshold | Severity | Action |
|-------|-----------|----------|--------|
| Database pool usage | ≥80% for 2 min | Warning | Investigate connection leaks |
| Database pool exhausted | 100% | Critical | Scale or restart |
| Error rate | >5% for 2 min | Critical | Check logs, investigate |
| Response time | P95 >2s | Warning | Check database performance |
| Circuit breaker open | Any | Warning | Check Horizon API status |
| Replication lag | >10 MB | Warning | Check replica health |
| DLQ entries | >100 | Warning | Investigate failed transactions |
| Disk usage | >80% | Warning | Archive old partitions |

### Log Monitoring

#### Important Log Patterns
```bash
# Connection pool warnings
grep "Database connection pool usage high" /var/log/synapse-core/app.log

# Circuit breaker events
grep "circuit breaker" /var/log/synapse-core/app.log

# Database errors
grep "Database error" /var/log/synapse-core/app.log

# DLQ movements
grep "moved to DLQ" /var/log/synapse-core/app.log

# Partition maintenance
grep "partition" /var/log/synapse-core/app.log
```

---

## Incident Response

### Incident Classification

| Severity | Definition | Response Time | Examples |
|----------|------------|---------------|----------|
| P0 - Critical | Service down, data loss | Immediate | Database unavailable, app crash |
| P1 - High | Degraded service | 15 minutes | High error rate, slow responses |
| P2 - Medium | Partial impact | 1 hour | Single component failure |
| P3 - Low | Minor issue | 4 hours | Non-critical feature broken |

### Investigating an Incident with a Trace ID

Every transaction created from an inbound webhook carries a `trace_id`
(`transactions.trace_id`, propagated from the request's `traceparent`
header — see `src/handlers/webhook.rs`). Use it to follow one logical
transaction across the pipeline instead of correlating records by
timestamp:

1. **Find the transaction:**
   ```sql
   SELECT * FROM transactions WHERE trace_id = '<trace-id>';
   ```
2. **Find every status change and who/what made it:**
   ```sql
   SELECT * FROM audit_logs WHERE trace_id = '<trace-id>' ORDER BY created_at;
   ```
   (Populated by `AuditLog::log_status_change_traced` — currently wired up
   at the admin `force_complete_transaction` mutation; not every audit call
   site in the codebase sets `trace_id` yet.)
3. **Find related reconciliation discrepancies:** a reconciliation report's
   `missing_on_chain`, `amount_mismatches`, and `late_payments` entries each
   carry the `trace_id` of the transaction they reference, so a report's
   `report_json` can be grepped for the trace ID directly.
4. **Correlate with structured logs:** `TransactionProcessor::process_transaction`
   records `trace_id` on its `processor.process_transaction` tracing span,
   so every `validate`/`enrich`/`verify`/`complete` stage log line for that
   run includes it in structured log output — filter your log aggregator by
   `trace_id="<trace-id>"`.

#### 1. Application Crash
**Symptoms:** Health check fails, no response from service

**Response:**
1. Check application status
   ```bash
   systemctl status synapse-core
   docker ps | grep synapse-core
   ```

2. Check recent logs for panic/crash
   ```bash
   journalctl -u synapse-core -n 100
   docker logs synapse-core --tail 100
   ```

3. Restart application
   ```bash
   systemctl restart synapse-core
   # Or
   docker-compose restart app
   ```

4. Verify health
   ```bash
   curl http://localhost:3000/health
   ```

**Estimated Recovery Time:** 2-5 minutes

#### 2. Database Connection Failure
**Symptoms:** `db_primary: "disconnected"` in health check

**Response:**
1. Check database status
   ```bash
   psql $DATABASE_URL -c "SELECT 1;"
   ```

2. Check database logs
   ```bash
   # For Docker
   docker logs synapse-postgres --tail 50
   
   # For native PostgreSQL
   tail -50 /var/log/postgresql/postgresql-14-main.log
   ```

3. If database is down, restart it
   ```bash
   docker-compose restart postgres
   # Or
   systemctl restart postgresql
   ```

4. If database is up but unreachable, check network/firewall

5. Verify application reconnects automatically (exponential backoff)

**Estimated Recovery Time:** 5-10 minutes

#### 3. High Error Rate
**Symptoms:** >5% 5xx errors for 2+ minutes

**Response:**
1. Check application logs for errors
   ```bash
   grep "ERROR" /var/log/synapse-core/app.log | tail -50
   ```

2. Check database performance
   ```sql
   -- Check for blocking queries
   SELECT * FROM pg_stat_activity WHERE wait_event_type IS NOT NULL;
   
   -- Check for long-running queries
   SELECT pid, now() - query_start AS duration, query 
   FROM pg_stat_activity 
   WHERE state = 'active' 
   ORDER BY duration DESC LIMIT 10;
   ```

3. Check connection pool status
   ```bash
   curl http://localhost:3000/health | jq '.db_pool'
   ```

4. Check circuit breaker state
   ```bash
   grep "circuit breaker" /var/log/synapse-core/app.log | tail -10
   ```

5. If Horizon API is down, circuit breaker will handle it automatically

**Estimated Recovery Time:** 10-30 minutes

#### 4. Redis Failure
**Symptoms:** Idempotency errors, cache misses

**Response:**
1. Check Redis status
   ```bash
   redis-cli -u $REDIS_URL ping
   ```

2. Check Redis logs
   ```bash
   docker logs synapse-redis --tail 50
   ```

3. Restart Redis if needed
   ```bash
   docker-compose restart redis
   ```

4. Application will experience cache misses and repopulate from database

**Estimated Recovery Time:** 5-10 minutes

#### 5. Circuit Breaker Open
**Symptoms:** Horizon API calls failing, circuit breaker open

**Response:**
1. Check Horizon API status
   ```bash
   curl https://horizon-testnet.stellar.org/
   ```

2. Check Stellar status page: https://status.stellar.org/

3. Circuit breaker will automatically retry after timeout (60-120s)

4. If Horizon is down, wait for recovery (no action needed)

5. Monitor logs for circuit breaker state changes
   ```bash
   grep "circuit breaker" /var/log/synapse-core/app.log -f
   ```

**Estimated Recovery Time:** Automatic (60-120s after Horizon recovers)

#### 5a. Triaging a Reported Duplicate Action from a Past Redis Outage
**Symptoms:** A customer reports an action (e.g. a callback-triggered transaction) that appears to have happened twice, and the timing correlates with a known Redis outage window.

**Why this can happen:** During a Redis outage, `IdempotencyService` falls back to a database-backed record (`idempotency_keys` table). The healthy-path Redis lookup now also consults that table on a Redis miss (fixed in Part B of the accompanying PR — previously it did not, which meant a retry arriving after Redis recovered could double-execute). Check `idempotency_db_fallback_recovered_total` for how often this recovery path is actually engaging.

**Response:**
1. Check whether the idempotency key in question has a DB-fallback record:
   ```sql
   SELECT key, status, response, created_at, expires_at
   FROM idempotency_keys
   WHERE key = '<the idempotency key from the customer report>';
   ```
2. If a row exists with `status = 'completed'`: the DB fallback worked as designed — a retry with this key should have been (and, after the Part B fix, will be) recognized rather than re-executed. If the duplicate report predates this fix's deployment, that's the expected pre-fix behavior for this exact window; no further action needed beyond confirming the fix is deployed.
3. If no row exists: the duplicate wasn't caused by this mechanism — look elsewhere (client-side retry without an idempotency key, a different bug).
4. Cross-reference the Redis outage window against `idempotency_db_fallback_recovered_total`'s timeseries to confirm the outage actually caused fallback activity around the reported time.

**Estimated Recovery Time:** N/A (investigation only).

#### 6. Missing-Partition (23514) Burst on Transaction Insert
**Symptoms:** A burst of `transaction_insert_missing_partition_total` counter increments, or `23514`/`no partition of relation found for row` errors in logs — typically right after a fresh deploy or a stalled `PartitionManager` maintenance loop, when a burst of transactions lands in a calendar month nobody has created a partition for yet.

**What happens automatically:** `insert_transaction` catches the `23514` error, calls `ensure_partition_for(created_at)` synchronously (which serializes concurrent callers targeting the same partition via `pg_advisory_xact_lock` and tolerates a losing concurrent `CREATE TABLE`), and retries the insert once. This should resolve itself within one request's latency — check `partition_self_heal_duration_ms` for how long the self-heal + lock wait actually took.

**Response:**
1. Confirm the self-heal is actually resolving it (not looping):
   ```bash
   grep "self-healed missing partition\|ensure_partition_for" /var/log/synapse-core/app.log | tail -50
   ```
2. Check whether the expected partition now exists:
   ```sql
   SELECT relname FROM pg_class WHERE relname LIKE 'transactions_y%' ORDER BY relname DESC LIMIT 5;
   ```
3. If `transaction_insert_missing_partition_total` keeps climbing with no corresponding drop in error rate, the advisory lock may be held by a stuck session — check for long-held locks:
   ```sql
   SELECT pid, mode, granted, query, now() - query_start AS held_for
   FROM pg_locks l JOIN pg_stat_activity a ON l.pid = a.pid
   WHERE l.locktype = 'advisory'
   ORDER BY held_for DESC;
   ```
   A session holding the lock past a few hundred milliseconds under normal DDL is anomalous — consider `SELECT pg_terminate_backend(<pid>)` on a confirmed-stuck session only, after checking it isn't mid-commit.
4. If partitions are chronically missing (not just a one-off burst), check whether the scheduled `PartitionManager` maintenance loop (24h interval by default, `main.rs`) is actually running — it creates partitions 2 months ahead of need, so a working self-heal masking a broken scheduled loop is itself worth fixing.

**Estimated Recovery Time:** Automatic, sub-second per affected insert; investigate if it recurs across multiple deploys.

#### 7. Duplicate Reconciliation Report Alert
**Symptoms:** More than one `reconciliation_reports` row for what should be the same daily window, or a spike in `reconciliation_duplicate_report_prevented_total`.

**What this means:** As of the `reconciliation_reports_period_unique` constraint, a genuine duplicate insert for the identical `(period_start, period_end)` is rejected at the database level and counted, not silently stored twice. So:
- A rising `reconciliation_duplicate_report_prevented_total` is **expected background noise** if `LeaderElection` ever fails open (e.g. a Redis blip) and two instances both ran the job for the same day — the constraint did its job.
- **An actual duplicate report row existing** (two rows with different `id` but the same `period_start`/`period_end`) would mean a regression — that should be impossible with the constraint in place.

**Response:**
1. Check whether it's the expected counter (no duplicate row) or an actual duplicate row:
   ```sql
   SELECT period_start, period_end, COUNT(*) FROM reconciliation_reports
   GROUP BY period_start, period_end HAVING COUNT(*) > 1;
   ```
2. If that query returns nothing: no regression, just check why `LeaderElection` failed open as often as the counter suggests (Redis health, network partition between instances and Redis).
3. If that query returns rows: this is a real regression — check that the migration `20260823000001_reconciliation_reports_unique_period.sql` is actually applied (`SELECT conname FROM pg_constraint WHERE conname = 'reconciliation_reports_period_unique';`), and file an incident, since the safety net that should make this impossible did not hold.

**Estimated Recovery Time:** N/A (detection/triage only — no auto-remediation for an actual constraint-bypass regression).

#### 8. Webhook Delivery DLQ Review
**Symptoms:** Rows appearing in `webhook_delivery_dlq` for the first time (this table has been reachable but never actually populated in production until the `webhook_enqueue_on_completion` flag is enabled — see `docs/webhook-delivery-architecture.md`), or a drop in `webhook_delivery_total{outcome="success"}`.

**What a DLQ row means:** A delivery exhausted `MAX_ATTEMPTS` (5) retries with exponential backoff. The endpoint has been failing consistently — check whether its circuit breaker is open too (`webhook_circuit_breaker_transitions_total{transition="opened"}`).

**Response:**
1. Review pending DLQ entries and why they failed:
   ```sql
   SELECT id, delivery_id, endpoint_id, transaction_id, event_type,
          attempt_count, last_response_status, last_response_body, created_at
   FROM webhook_delivery_dlq
   ORDER BY created_at DESC
   LIMIT 50;
   ```
2. Check whether the failure is endpoint-side (4xx/5xx from `last_response_status`, endpoint URL unreachable) or ours (payload/signature issue — compare `last_response_body` against the expected error format for that tenant's integration).
3. If the endpoint has since been fixed and should receive the missed event, replay it. **There is currently no admin HTTP endpoint for this** — `WebhookDispatcher::replay_from_dlq(dlq_id)` exists in code (`src/services/webhook_dispatcher.rs`) but must be invoked from a Rust context against the running database (e.g. a one-off binary, or a REPL-style task), not via `curl`. This is a known gap; adding an admin route is a reasonable follow-up.
4. If the endpoint is broken and won't be fixed soon, no action needed beyond notifying the tenant — the DLQ row is a durable record; it doesn't need manual cleanup.
5. If DLQ rows are appearing across *many* endpoints at once rather than one bad endpoint, suspect an us-side regression (payload shape change, signature bug) rather than N simultaneous endpoint outages — check `webhook_delivery_total{outcome="failure"}` broken down by endpoint_id to confirm it's broad, not concentrated.

**Estimated Recovery Time:** Investigation only; replay is manual (see step 3) until an admin endpoint exists.

### Escalation Procedures

#### Tier 1 (Automated)
- Alert sent to Slack/PagerDuty
- On-call engineer notified

#### Tier 2 (15 minutes unresolved)
- Escalate to secondary on-call
- Notify team lead

#### Tier 3 (30 minutes unresolved)
- Escalate to engineering director
- Consider multi-region failover

---

## Maintenance Tasks

### Daily Tasks

#### 1. Health Check Review
```bash
# Check application health
curl http://localhost:3000/health | jq

# Review error logs
grep "ERROR\|WARN" /var/log/synapse-core/app.log | tail -50

# Check DLQ entries
curl http://localhost:3000/dlq | jq '.count'
```

#### 2. Monitor Key Metrics
- Connection pool usage
- Error rates
- Response times
- DLQ entry count

### Weekly Tasks

#### 1. Review DLQ Entries
```bash
# List DLQ entries
curl http://localhost:3000/dlq | jq

# Investigate error reasons
# Requeue if transient errors resolved
curl -X POST http://localhost:3000/dlq/{id}/requeue
```

#### 2. Database Performance Review
```sql
-- Check slow queries
SELECT query, calls, mean_exec_time, max_exec_time
FROM pg_stat_statements
ORDER BY mean_exec_time DESC
LIMIT 10;

-- Check table sizes
SELECT 
    schemaname,
    tablename,
    pg_size_pretty(pg_total_relation_size(schemaname||'.'||tablename)) AS size
FROM pg_tables
WHERE schemaname = 'public'
ORDER BY pg_total_relation_size(schemaname||'.'||tablename) DESC;
```

#### 3. Review Audit Logs
```sql
-- Check recent audit activity
SELECT entity_type, action, COUNT(*) 
FROM audit_logs 
WHERE timestamp > NOW() - INTERVAL '7 days'
GROUP BY entity_type, action
ORDER BY COUNT(*) DESC;

-- Check for suspicious patterns
SELECT actor, COUNT(*) 
FROM audit_logs 
WHERE timestamp > NOW() - INTERVAL '7 days'
GROUP BY actor
ORDER BY COUNT(*) DESC;
```

### Monthly Tasks

#### 1. Partition Maintenance Verification
```sql
-- Verify partitions exist for next 2 months
SELECT 
    c.relname AS partition_name,
    pg_get_expr(c.relpartbound, c.oid) AS partition_bound
FROM pg_class c
JOIN pg_inherits i ON c.oid = i.inhrelid
JOIN pg_class p ON i.inhparent = p.oid
WHERE p.relname = 'transactions'
ORDER BY c.relname DESC
LIMIT 3;

-- Manually create if missing
SELECT create_monthly_partition();
```

#### 2. Archive Old Partitions
```sql
-- List partitions older than 12 months
SELECT 
    c.relname AS partition_name,
    pg_size_pretty(pg_total_relation_size(c.oid)) AS size
FROM pg_class c
WHERE c.relname LIKE 'transactions_y%'
  AND c.relname NOT IN (
      SELECT c2.relname 
      FROM pg_class c2
      JOIN pg_inherits i ON c2.oid = i.inhrelid
  )
ORDER BY c.relname;

-- Export to archive
COPY transactions_y2024m01 TO '/archive/transactions_2024_01.csv' CSV HEADER;

-- Drop after backup verification
DROP TABLE transactions_y2024m01;
```

#### 3. Database Vacuum and Analyze
```sql
-- Vacuum and analyze all tables
VACUUM ANALYZE;

-- Check bloat
SELECT 
    schemaname,
    tablename,
    pg_size_pretty(pg_total_relation_size(schemaname||'.'||tablename)) AS size
FROM pg_tables
WHERE schemaname = 'public'
ORDER BY pg_total_relation_size(schemaname||'.'||tablename) DESC;
```

#### 4. Security Review
- Review audit logs for suspicious activity
- Rotate database credentials
- Review access logs
- Update dependencies (`cargo update`)

### Quarterly Tasks

#### 1. Disaster Recovery Test
- Test database backup restoration
- Test multi-region failover
- Verify backup integrity
- Update runbooks based on findings

#### 2. Performance Tuning
- Review slow query logs
- Optimize indexes
- Adjust connection pool sizes
- Review partition strategy

#### 3. Capacity Planning
- Review growth trends
- Plan for scaling needs
- Evaluate infrastructure costs
- Update capacity forecasts

---

## Troubleshooting

### Connection Pool Issues

#### Symptom: Pool usage consistently high (>80%)
**Diagnosis:**
```sql
-- Check for long-running queries
SELECT pid, now() - query_start AS duration, query, state
FROM pg_stat_activity
WHERE state = 'active'
ORDER BY duration DESC;

-- Check for idle in transaction
SELECT pid, now() - state_change AS duration, query, state
FROM pg_stat_activity
WHERE state = 'idle in transaction'
ORDER BY duration DESC;
```

**Resolution:**
1. Kill long-running queries if safe
   ```sql
   SELECT pg_terminate_backend(pid);
   ```

2. Increase pool size (short-term)
3. Fix connection leaks in code (long-term)
4. Scale horizontally (add more instances)

#### Symptom: Pool exhaustion (100% usage)
**Diagnosis:**
```bash
# Check health endpoint
curl http://localhost:3000/health | jq '.db_pool'

# Check application logs
grep "pool timeout" /var/log/synapse-core/app.log
```

**Resolution:**
1. Restart application (immediate)
   ```bash
   systemctl restart synapse-core
   ```

2. Investigate root cause
3. Increase pool size if needed
4. Fix code issues causing leaks

### Database Performance Issues

#### Symptom: Slow query performance
**Diagnosis:**
```sql
-- Enable query timing
\timing

-- Check for missing indexes
SELECT schemaname, tablename, attname, n_distinct, correlation
FROM pg_stats
WHERE schemaname = 'public'
ORDER BY abs(correlation) DESC;

-- Check index usage
SELECT 
    schemaname,
    tablename,
    indexname,
    idx_scan,
    idx_tup_read,
    idx_tup_fetch
FROM pg_stat_user_indexes
WHERE schemaname = 'public'
ORDER BY idx_scan ASC;
```

**Resolution:**
1. Add missing indexes
2. Vacuum and analyze tables
3. Update table statistics
4. Consider query optimization

#### Symptom: High replication lag
**Diagnosis:**
```sql
-- Check replication lag
SELECT 
    client_addr,
    state,
    sync_state,
    pg_wal_lsn_diff(pg_current_wal_lsn(), replay_lsn) AS lag_bytes,
    pg_size_pretty(pg_wal_lsn_diff(pg_current_wal_lsn(), replay_lsn)) AS lag
FROM pg_stat_replication;
```

**Resolution:**
1. Check replica server resources (CPU, disk I/O)
2. Check network bandwidth between primary and replica
3. Consider increasing `max_wal_senders` on primary
4. Temporarily route reads to primary if critical

### Application Issues

#### Symptom: High memory usage
**Diagnosis:**
```bash
# Check memory usage
ps aux | grep synapse-core

# Check for memory leaks in logs
grep "OOM\|memory" /var/log/synapse-core/app.log
```

**Resolution:**
1. Restart application (immediate)
2. Review recent code changes
3. Profile application with memory profiler
4. Increase memory limits if needed

#### Symptom: High CPU usage
**Diagnosis:**
```bash
# Check CPU usage
top -p $(pgrep synapse-core)

# Check for CPU-intensive queries
SELECT query, calls, total_exec_time, mean_exec_time
FROM pg_stat_statements
ORDER BY total_exec_time DESC
LIMIT 10;
```

**Resolution:**
1. Identify CPU-intensive operations
2. Optimize hot code paths
3. Scale horizontally if needed
4. Consider caching frequently computed results

### DLQ Issues

#### Symptom: High DLQ entry count
**Diagnosis:**
```bash
# Check DLQ entries
curl http://localhost:3000/dlq | jq

# Group by error reason
curl http://localhost:3000/dlq | jq '.dlq_entries | group_by(.error_reason) | map({error: .[0].error_reason, count: length})'
```

**Resolution:**
1. Investigate common error reasons
2. Fix underlying issues (e.g., validation, Horizon API)
3. Requeue entries after fixes
   ```bash
   curl -X POST http://localhost:3000/dlq/{id}/requeue
   ```

4. Monitor for recurrence

---

## Deployment Procedures

### Pre-Deployment Checklist

- [ ] Review changes in staging environment
- [ ] Run full test suite (`cargo test`)
- [ ] Check for database migrations
- [ ] Review rollback plan
- [ ] Notify team of deployment window
- [ ] Verify backup is recent (<24 hours)

### Standard Deployment

#### 1. Backup Current State
```bash
# Backup database
pg_dump $DATABASE_URL > backup_$(date +%Y%m%d_%H%M%S).sql

# Tag current version
git tag -a v$(date +%Y%m%d_%H%M%S) -m "Pre-deployment backup"
```

#### 2. Deploy Application
```bash
# Pull latest code
git pull origin main

# Build release binary
cargo build --release

# Stop application
systemctl stop synapse-core

# Replace binary
cp target/release/synapse-core /usr/local/bin/

# Start application (migrations run automatically)
systemctl start synapse-core
```

#### 3. Verify Deployment
```bash
# Check health
curl http://localhost:3000/health

# Check logs
journalctl -u synapse-core -f

# Monitor error rates
watch -n 5 'curl -s http://localhost:3000/health | jq'
```

### Docker Deployment

```bash
# Pull latest images
docker-compose pull

# Stop services
docker-compose down

# Start services (migrations run automatically)
docker-compose up -d

# Verify health
docker-compose ps
curl http://localhost:3000/health
```

### Rollback Procedure

#### If deployment fails:
```bash
# Stop application
systemctl stop synapse-core

# Restore previous binary
cp /backup/synapse-core /usr/local/bin/

# Rollback database migrations (if needed)
sqlx migrate revert

# Start application
systemctl start synapse-core

# Verify health
curl http://localhost:3000/health
```

### Database Migration Deployment

#### For migrations with downtime:
1. Enable maintenance mode
2. Stop application
3. Run migrations manually
   ```bash
   sqlx migrate run
   ```
4. Start application
5. Disable maintenance mode

#### For zero-downtime migrations:
1. Deploy backward-compatible schema changes
2. Deploy application code
3. Run data migration in background
4. Deploy cleanup migration

---

## Security Operations

### Access Control

#### Database Access
- Use separate credentials for application and admin access
- Rotate credentials quarterly
- Use SSL/TLS for all database connections
- Restrict database access by IP whitelist

#### Application Access
- Use API keys for webhook authentication
- Rotate API keys regularly
- Monitor for unauthorized access attempts
- Implement rate limiting

### Audit Log Review

#### Daily Review
```sql
-- Check for unusual activity
SELECT entity_type, action, actor, COUNT(*)
FROM audit_logs
WHERE timestamp > NOW() - INTERVAL '24 hours'
GROUP BY entity_type, action, actor
ORDER BY COUNT(*) DESC;
```

#### Weekly Review
```sql
-- Check for suspicious patterns
SELECT 
    actor,
    entity_type,
    COUNT(*) as action_count,
    COUNT(DISTINCT entity_id) as unique_entities
FROM audit_logs
WHERE timestamp > NOW() - INTERVAL '7 days'
GROUP BY actor, entity_type
HAVING COUNT(*) > 1000
ORDER BY action_count DESC;
```

### Security Incident Response

#### If unauthorized access detected:
1. Immediately rotate all credentials
2. Review audit logs for affected entities
3. Notify security team
4. Investigate attack vector
5. Implement additional security controls
6. Document incident and response

---

## Backup & Recovery

### Backup Strategy

#### Automated Backups
- **Frequency**: Daily at 2 AM UTC
- **Retention**: 30 days
- **Location**: Remote storage (AWS S3, Azure Blob)
- **Type**: Full database dump

#### Manual Backup
```bash
# Full database backup
pg_dump $DATABASE_URL > backup_$(date +%Y%m%d_%H%M%S).sql

# Compressed backup
pg_dump $DATABASE_URL | gzip > backup_$(date +%Y%m%d_%H%M%S).sql.gz

# Upload to S3
aws s3 cp backup_$(date +%Y%m%d_%H%M%S).sql.gz s3://synapse-backups/
```

### Recovery Procedures

#### Complete Database Recovery
**Estimated Time:** 30-45 minutes

1. Stop application traffic
   ```bash
   systemctl stop synapse-core
   ```

2. Download latest backup
   ```bash
   aws s3 cp s3://synapse-backups/latest.sql.gz .
   gunzip latest.sql.gz
   ```

3. Restore database
   ```bash
   # Drop existing database (if needed)
   dropdb synapse
   createdb synapse
   
   # Restore from backup
   psql $DATABASE_URL < latest.sql
   ```

4. Verify data integrity
   ```sql
   SELECT COUNT(*) FROM transactions;
   SELECT MAX(created_at) FROM transactions;
   ```

5. Start application
   ```bash
   systemctl start synapse-core
   ```

6. Verify health
   ```bash
   curl http://localhost:3000/health
   ```

#### Partial Table Recovery
**Estimated Time:** 15-30 minutes

1. Restore backup to temporary database
   ```bash
   createdb synapse_temp
   psql postgres://localhost/synapse_temp < backup.sql
   ```

2. Export specific table
   ```bash
   pg_dump -t transactions postgres://localhost/synapse_temp > transactions.sql
   ```

3. Import to production
   ```bash
   psql $DATABASE_URL < transactions.sql
   ```

4. Verify data
   ```sql
   SELECT COUNT(*) FROM transactions;
   ```

#### Point-in-Time Recovery (PITR)
If using continuous archiving:

```bash
# Restore to specific timestamp
pg_restore --target-time='2024-02-20 10:30:00' backup.dump
```

### Backup Verification

#### Monthly Verification
1. Download random backup
2. Restore to test environment
3. Verify data integrity
4. Test application functionality
5. Document results

---

## Emergency Contacts

### On-Call Rotation
- **Primary**: Check PagerDuty schedule
- **Secondary**: Check PagerDuty schedule
- **Escalation**: Engineering Director

### External Services
- **Stellar Status**: https://status.stellar.org/
- **AWS Support**: [Support Portal]
- **Database Provider**: [Support Contact]

### Communication Channels
- **Incidents**: #incidents Slack channel
- **Alerts**: #alerts Slack channel
- **Status Page**: [Status Page URL]

---

## Appendix

### Useful Commands

#### Application Management
```bash
# Start service
systemctl start synapse-core

# Stop service
systemctl stop synapse-core

# Restart service
systemctl restart synapse-core

# View logs
journalctl -u synapse-core -f

# Check status
systemctl status synapse-core
```

#### Database Management
```bash
# Connect to database
psql $DATABASE_URL

# Run migrations
sqlx migrate run

# Revert last migration
sqlx migrate revert

# Check migration status
sqlx migrate info
```

#### Docker Management
```bash
# Start all services
docker-compose up -d

# Stop all services
docker-compose down

# View logs
docker-compose logs -f

# Restart specific service
docker-compose restart app
```

### Configuration Files

- **Application**: `.env`
- **Docker**: `docker-compose.yml`
- **Database**: `migrations/`
- **Systemd**: `/etc/systemd/system/synapse-core.service`

### Monitoring Dashboards

- **Application Metrics**: [Grafana URL]
- **Database Metrics**: [Database Dashboard URL]
- **Logs**: [Log Aggregation URL]
- **Alerts**: [PagerDuty URL]

---

## Document Maintenance

**Last Updated:** 2024-02-20  
**Version:** 1.0  
**Owner:** Platform Team  
**Review Schedule:** Quarterly

### Change Log

| Date | Version | Changes | Author |
|------|---------|---------|--------|
| 2024-02-20 | 1.0 | Initial runbook creation | Platform Team |

### Feedback

For runbook improvements or corrections, please:
1. Open an issue in the repository
2. Submit a pull request with changes
3. Contact the platform team in #platform Slack channel
