# Swap Data Store — Backup, Restore, and DR Runbook (#1477)

This document covers backup automation, point-in-time restore, and disaster-recovery drills specifically for the PostgreSQL and Redis stores that hold Phase 2 swap data (swap orders, quotes, fills, and events).

**RTO target:** 30 minutes  
**RPO target:** 5 minutes (WAL `archive_timeout = 300 s`)

## Table of Contents

1. [Architecture Overview](#architecture-overview)
2. [PostgreSQL WAL Archiving Setup](#postgresql-wal-archiving-setup)
3. [Redis Backup Configuration](#redis-backup-configuration)
4. [Scheduled Backup Automation](#scheduled-backup-automation)
5. [Restore Procedure](#restore-procedure)
6. [DR Drill Procedure](#dr-drill-procedure)
7. [Validating Swap Table Integrity](#validating-swap-table-integrity)
8. [Encryption and Access Control](#encryption-and-access-control)
9. [Retention Policy](#retention-policy)
10. [Handling Schema-Version Skew](#handling-schema-version-skew)

---

## Architecture Overview

```
PostgreSQL primary
  ├── Continuous WAL archiving → WAL_ARCHIVE_DIR (local or S3)
  └── Scheduled base backup → BACKUP_DIR (scripts/backup-swap-data.sh)
         └── Encrypted: AES-256-CBC, PBKDF2 key derivation
         └── Checksummed: SHA-256

Redis
  └── RDB snapshot → encrypted copy in BACKUP_DIR
```

The backup key (`BACKUP_KEY`) must be stored in a secrets manager (Vault / AWS Secrets Manager) and injected at runtime. It must never be committed to source control or hard-coded in configuration files.

---

## PostgreSQL WAL Archiving Setup

WAL archiving provides point-in-time recovery (PITR) and bounds the RPO to the `archive_timeout` value.

### postgresql.conf settings

```conf
wal_level = replica
archive_mode = on
archive_command = 'cp %p /data/wal_archive/%f'
archive_timeout = 300          # 5-minute RPO ceiling
wal_compression = on           # Reduce archive storage
```

### Verify archiving is active

```bash
psql $DATABASE_URL -c "SELECT pg_walfile_name(pg_current_wal_lsn()), now();"
ls -lth /data/wal_archive/ | head -5
```

### WAL continuity check

```bash
ls /data/wal_archive/ | wc -l   # Should increase every 5 minutes
```

---

## Redis Backup Configuration

Redis swap-domain data (idempotency cache, circuit-breaker state) is non-authoritative — the PostgreSQL swap tables are the system of record. Nevertheless, backups reduce warm-up time after a Redis failure.

### Recommended settings (redis.conf)

```conf
save 900 1          # RDB snapshot after 15 minutes if ≥1 key changed
save 300 10         # After 5 minutes if ≥10 keys changed
appendonly yes      # AOF for durability between RDB snapshots
appendfsync everysec
```

### Backup via scripts/backup-swap-data.sh

The script reads the RDB file path from `REDIS_RDB_PATH` (default: `/var/lib/redis/dump.rdb`) and encrypts it alongside the PostgreSQL base backup.

---

## Scheduled Backup Automation

Configure cron or a systemd timer to run the backup script:

```cron
# /etc/cron.d/synapse-swap-backup

# Hourly base backup (lightweight: WAL-only since last full)
0 * * * *  synapse  BACKUP_KEY=$(vault kv get -field=key secret/synapse/backup) \
                     BACKUP_DIR=/data/backups WAL_ARCHIVE_DIR=/data/wal_archive \
                     DATABASE_URL=$DATABASE_URL \
                     /app/scripts/backup-swap-data.sh --type hourly

# Daily full base backup
0 2 * * *  synapse  BACKUP_KEY=$(vault kv get -field=key secret/synapse/backup) \
                     BACKUP_DIR=/data/backups WAL_ARCHIVE_DIR=/data/wal_archive \
                     DATABASE_URL=$DATABASE_URL \
                     /app/scripts/backup-swap-data.sh --type daily --verify

# Monthly archive snapshot
0 3 1 * *  synapse  BACKUP_KEY=$(vault kv get -field=key secret/synapse/backup) \
                     BACKUP_DIR=/data/backups WAL_ARCHIVE_DIR=/data/wal_archive \
                     DATABASE_URL=$DATABASE_URL \
                     /app/scripts/backup-swap-data.sh --type monthly --verify
```

---

## Restore Procedure

**Estimated RTO: 30 minutes.**

### Pre-restore checklist

- [ ] Identify the target point in time (PIT) or the specific backup archive.
- [ ] Confirm the restore target is an empty or isolated database.
- [ ] Retrieve `BACKUP_KEY` from the secrets manager.
- [ ] Verify the backup archive checksum before starting.

### Step-by-step

```bash
# 1. Retrieve backup key
export BACKUP_KEY=$(vault kv get -field=key secret/synapse/backup)

# 2. Point to the clean restore target
export TARGET_DATABASE_URL="postgres://synapse:password@restore-host:5432/synapse_restore"

# 3. Run the restore script
./scripts/restore-swap-data.sh \
  --archive /data/backups/backup-daily-20261024T020000Z.tar.gz.enc

# 4. For a corrupted or untrustworthy archive, verify first
./scripts/restore-swap-data.sh \
  --archive /data/backups/backup-daily-20261024T020000Z.tar.gz.enc \
  --verify-only
```

### PITR (point-in-time restore)

```bash
# 1. Restore the closest base backup before your target time
./scripts/restore-swap-data.sh \
  --archive /data/backups/backup-daily-20261024T020000Z.tar.gz.enc

# 2. Configure recovery target in recovery.conf (PostgreSQL 12 and below)
#    or postgresql.conf (PostgreSQL 13+):
cat > /pgdata/recovery.conf << 'EOF'
restore_command = 'cp /data/wal_archive/%f %p'
recovery_target_time = '2026-10-24 14:30:00 UTC'
recovery_target_action = 'promote'
EOF

# 3. Start PostgreSQL in recovery mode; it replays WAL until the target time
pg_ctl start -D /pgdata
```

---

## DR Drill Procedure

Run a documented restore drill at least once per quarter. Record:
- Date and operator
- Source backup used (run_id)
- Measured RTO (time from drill start to validated restore)
- Row counts for swap_orders and swap_quotes
- Any deviations from this procedure

```bash
# Run the automated drill test (Requires pg_dump, psql, openssl)
DR_DRILL_DATABASE_URL=$DATABASE_URL \
  cargo test --test backup_restore_drill_test -- --ignored test_restore_drill_validates_swap_table_integrity

# Or run the full script against a throwaway database
TARGET_DATABASE_URL="postgres://synapse:password@localhost:5434/drill_db" \
BACKUP_KEY=$(vault kv get -field=key secret/synapse/backup) \
./scripts/restore-swap-data.sh \
  --archive /data/backups/backup-daily-LATEST.tar.gz.enc
```

The drill PASSES when:
1. Checksum verification succeeds.
2. All 4 swap domain tables are present in the restored database.
3. `swap_orders` and `swap_quotes` row counts are non-zero (if the source had data).
4. No migration errors are reported.

---

## Validating Swap Table Integrity

After any restore, run:

```sql
-- All 4 swap domain tables must exist
SELECT table_name
FROM information_schema.tables
WHERE table_name IN ('swap_orders','swap_quotes','swap_fills','swap_events')
  AND table_schema = 'public'
ORDER BY table_name;

-- Row counts (verify against expected values from the backup metadata)
SELECT 'swap_orders' AS tbl, COUNT(*) FROM swap_orders
UNION ALL
SELECT 'swap_quotes',        COUNT(*) FROM swap_quotes
UNION ALL
SELECT 'swap_fills',         COUNT(*) FROM swap_fills
UNION ALL
SELECT 'swap_events',        COUNT(*) FROM swap_events;

-- No partial rows (required fields non-null)
SELECT COUNT(*) AS partial_orders
FROM swap_orders
WHERE source_asset IS NULL OR source_amount IS NULL OR status IS NULL;

-- Check no orphaned fills (every fill should reference a valid order)
SELECT COUNT(*) AS orphaned_fills
FROM swap_fills f
WHERE NOT EXISTS (
    SELECT 1 FROM swap_orders o
    WHERE o.id = f.order_id AND o.created_at = f.order_created_at
);
```

---

## Encryption and Access Control

- **Algorithm**: AES-256-CBC with PBKDF2 key derivation (openssl `enc -pbkdf2`).
- **Key storage**: Vault KV v2 at `secret/synapse/backup`. Access granted to the `synapse-backup` Vault role only.
- **Key rotation**: rotate `BACKUP_KEY` quarterly; update the secret in Vault and re-encrypt any long-term monthly archives before the old key expires.
- **Network**: backup files should only be stored on volumes or object stores with server-side encryption enabled (S3 SSE-S3 minimum, SSE-KMS preferred).

---

## Retention Policy

| Backup type | Count kept | Approximate window |
|---|---|---|
| Hourly      | 24         | 1 day              |
| Daily       | 30         | 30 days            |
| Monthly     | 12         | 12 months          |

Old backups are removed by `scripts/backup-swap-data.sh` at the end of each run. Long-term archival (>12 months) should use cold storage (S3 Glacier or equivalent) per the organisation's data retention policy.

---

## Handling Schema-Version Skew

The restore script reads the last applied migration version from `_sqlx_migrations` on the target database after restoring. If the backup was taken during an in-progress migration (rare but possible):

1. Stop the restore.
2. Identify which migration was partially applied using the version logged.
3. Run the migration's `.down.sql` on the restored database to roll it back.
4. Re-apply the migration using `cargo sqlx migrate run`.
5. Re-validate swap table integrity.

Partitioned tables (`swap_orders`, `swap_quotes`, `swap_fills`, `swap_events`) are backed up with their partition DDL included in the SQL dump. Partition maintenance functions (`ensure_swap_partitions`, `detach_old_swap_partitions`) are also included and will run correctly after restore.
