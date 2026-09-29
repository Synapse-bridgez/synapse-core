# Disaster Recovery Runbook

This document outlines the disaster recovery procedures for the application, responding to the requirement for managing complete database recovery, Redis data loss, application crashes, and multi-region failovers.

## 1. Complete Database Recovery from Backup
**Estimated Recovery Time:** 30-45 minutes

### Procedure:
1. Identify the latest complete database backup from the automated remote backup storage (e.g., AWS S3).
2. Stop application traffic to the database to prevent partial writes during restoration.
3. Provision a new database instance or clean the existing database schema.
4. Download and restore the backup using the database restoration tool (e.g., `pg_restore` for PostgreSQL).
5. Verify data integrity and consistency by running automated database validation checklists.
6. Update connection strings in application configuration if a new instance was provisioned.
7. Resume application traffic and monitor database performance and error rates.

## 2. Partial Table Recovery
**Estimated Recovery Time:** 15-30 minutes

### Procedure:
1. Locate the latest backup that contains the uncorrupted table data.
2. Restore the backup to a temporary, isolated database instance.
3. Extract the required table data using an export tool (e.g., `pg_dump -t table_name`).
4. Import the extracted table into the production database.
5. Provide necessary data reconciliation to synchronize relations and foreign keys.
6. Verify the restored data against recent application metrics.

## 3. Redis Data Loss Recovery
**Estimated Recovery Time:** 5-10 minutes

### Procedure:
1. Identify the cause of the Redis failure (e.g., OOM, crash).
2. If using Redis persistence (RDB/AOF), restart the Redis service to attempt auto-recovery from the last reliable snapshot.
3. If the snapshot is corrupted or unavailable, flush the corrupted data completely.
4. Restart the Redis cluster or instance.
5. Applications relying on Redis caches will experience cache misses and will gradually repopulate the cache from the primary database. Monitor database load to ensure it handles cache warming securely.

## 4. Application Crash and Restart
**Estimated Recovery Time:** 2-5 minutes

### Procedure:
1. Identify the crashed application nodes through standard monitoring tools or health check failures.
2. Analyze the immediate system logs to ascertain causes such as configuration bugs, OOM events, or fatal panics.
3. Restart the application service or pods (e.g., via `systemctl restart synapse-core` or orchestrator commands).
4. Monitor startup logs to ensure the application successfully binds to external ports and re-establishes DB connections.
5. Verify health check endpoints return HTTP 200 OK.

## 5. Multi-Region Failover Procedure
**Estimated Recovery Time:** 15-20 minutes

### Automated execution

After incident command authorizes the operation, run the guarded playbook from
an operator workstation:

```bash
DNS_CUTOVER_COMMAND='your-dns-provider-command' \
DB_PROMOTION_COMMAND='your-replica-promotion-command' \
SERVICE_RESTART_COMMAND='kubectl -n synapse rollout restart deployment/synapse-core' \
HEALTHCHECK_URL='https://api.example.com' \
./scripts/dr-failover.sh --confirm-failover us-west-2
```

The command requires an exact typed confirmation, records every command and
timestamp under `var/dr-failover/`, and prints the post-failover checklist.
It never triggers unattended failover.

### Procedure:
1. Confirm the primary region is wholly unreachable or experiencing critical infrastructure failures.
2. Escalate to the incident response tier to officially authorize the failover operation.
3. Update global DNS routing records to re-route requests from the primary region to the disaster recovery secondary region.
4. Promote the secondary region's read-replica database to become the primary active master node.
5. Confirm corresponding application instances are scaled sufficiently to assume global traffic limits.
6. Provide elevated monitoring across secondary region systems until situation resolves.

## Monitoring Alerts and Escalation Procedures

## 6. Point-in-Time Recovery (PITR)
**Estimated Recovery Time:** 30-60 minutes (depending on WAL file size and recovery distance)

### Overview
Point-in-Time Recovery allows restoration of the database to **any specific moment in time** between the base backup and the latest archived WAL file. This is essential for recovering from data corruption, accidental deletes, or bad migrations.

### Prerequisites
- Base backup created with `pg_dump` (stored in `backup_dir`)
- Complete WAL file archive (stored in `wal_archive_dir`)
- Recovery target timestamp within WAL retention period

### Configuration
```
# In postgresql.conf:
archive_mode = on
archive_command = 'cp %p /path/to/wal_archive/%f'
archive_timeout = 300

# In synapse config:
pitr_config = PITRConfig {
    backup_dir: "/data/backups",
    wal_archive_dir: "/data/wal_archive",
    wal_retention_days: 30,
}
```

### Procedure
1. **Verify PITR Readiness**
   ```bash
   curl http://localhost:8000/health/pitr
   ```
   Check that WAL archiving is enabled and WAL files are available.

2. **Determine Recovery Target Time**
   - Identify the exact moment of data corruption/issue
   - Choose a timestamp 1-5 minutes before the issue
   - Format: ISO8601 (e.g., `2025-06-15T14:30:00Z`)

3. **Create Recovery Script**
   ```bash
   ./scripts/pitr_restore.sh "2025-06-15T14:30:00Z"
   ```

4. **Validate Recovery**
   ```bash
   # Connect to recovered database
   psql -c "SELECT COUNT(*) FROM affected_table;"
   # Verify data is in expected state
   ```

5. **Promote Recovered Database**
   - Run integration tests
   - Update application connection strings if needed
   - Resume application traffic

### Verifying WAL Continuity
Before initiating PITR, verify WAL files are complete:
```bash
# Check WAL archive
ls -la /data/wal_archive/ | head -20

# Verify no gaps
ls /data/wal_archive/ | wc -l  # Should be continuous
```

### Storage Growth Estimation
WAL files grow based on write volume:
- 1000 TPS × 300s checkpoint = ~30GB per day
- Retention of 30 days = ~900GB of WAL storage

## 7. Scheduled Disaster-Recovery Drill
Run a documented recovery drill at least once per quarter and after any major backup, failover, or schema workflow change. Record the drill date, operator, restore source, measured recovery time, and follow-up actions in the incident tracker.

### PITR Testing
Include monthly PITR test to verify:
```bash
# Test restore to 7 days ago
TARGET=$(date -u -d '7 days ago' +%Y-%m-%dT%H:%M:%SZ)
./scripts/test_pitr.sh "$TARGET"
```

### Key Alerts:
* **Database Connection Failure:** Triggered when DB does not respond to ping attempts > 30s.
* **Elevated Application Errors:** Triggered when 5xx errors > 5% for a consecutive 2-minute period.
* **Cache System Dropout:** Triggered on an inability to complete connection handshakes with Redis.
* **Component Crash:** Triggered when required components are down/offline.

### Escalation Workflow:
1. **Tier 1 (Automated):** Active alert broadcast sent to appropriate Slack channels and on-call respondent via automated pager.
2. **Tier 2 (15 mins unresolved):** Incident upgrades to the secondary point of contact and designated Team Leader.
3. **Tier 3 (30 mins unresolved):** Escalation immediately continues to System Administration leads and Engineering Directors to coordinate potential multi-region action.
