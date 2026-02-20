# Database Partitioning - Deployment Checklist

## Pre-Deployment

### Environment Verification
- [ ] PostgreSQL version is 14 or higher
- [ ] Database has sufficient storage for partition overhead (~5-10% extra)
- [ ] Backup of current database completed
- [ ] Test environment matches production PostgreSQL version

### Code Review
- [ ] All files reviewed and tested
- [ ] No syntax errors in SQL migration
- [ ] Rust code compiles without errors
- [ ] Unit tests pass
- [ ] Integration tests pass

### Documentation Review
- [ ] README.md updated with partitioning section
- [ ] docs/partitioning.md reviewed and accurate
- [ ] PR_DESCRIPTION.md complete
- [ ] Quick reference card available

## Deployment Steps

### 1. Pre-Migration Checks
```bash
# Check PostgreSQL version
psql -U synapse -d synapse -c "SELECT version();"

# Check current table size
psql -U synapse -d synapse -c "SELECT pg_size_pretty(pg_total_relation_size('transactions'));"

# Count existing records
psql -U synapse -d synapse -c "SELECT COUNT(*) FROM transactions;"

# Backup database
pg_dump -U synapse synapse > backup_before_partitioning_$(date +%Y%m%d).sql
```

### 2. Deploy Application
```bash
# Pull latest code
git checkout feature/issue-16-db-partitioning
git pull origin feature/issue-16-db-partitioning

# Build application
cargo build --release

# Run application (migrations run automatically)
cargo run --release
```

### 3. Verify Migration
```sql
-- Check partitions created
SELECT 
    c.relname AS partition_name,
    pg_get_expr(c.relpartbound, c.oid) AS partition_bound,
    pg_size_pretty(pg_total_relation_size(c.oid)) AS size
FROM pg_class c
JOIN pg_inherits i ON c.oid = i.inhrelid
JOIN pg_class p ON i.inhparent = p.oid
WHERE p.relname = 'transactions'
ORDER BY c.relname;

-- Expected: 3 partitions (y2025m02, y2025m03, y2025m04)

-- Verify data migrated
SELECT COUNT(*) FROM transactions;
SELECT COUNT(*) FROM transactions_old;
-- Both should have same count

-- Check partition manager started
-- Look for log: "Partition manager started"
```

### 4. Test Operations
```sql
-- Test INSERT
INSERT INTO transactions (stellar_account, amount, asset_code, created_at)
VALUES ('TEST123', 100.00, 'USD', NOW());

-- Verify it went to correct partition
SELECT tableoid::regclass, * FROM transactions WHERE stellar_account = 'TEST123';

-- Test partition functions
SELECT create_monthly_partition();
SELECT maintain_partitions();

-- Clean up test data
DELETE FROM transactions WHERE stellar_account = 'TEST123';
```

## Post-Deployment

### Monitoring Setup
- [ ] Set up partition size monitoring
- [ ] Configure alerts for partition maintenance failures
- [ ] Monitor application logs for partition manager messages
- [ ] Track query performance improvements

### Performance Validation
```sql
-- Run EXPLAIN ANALYZE on common queries
EXPLAIN ANALYZE
SELECT * FROM transactions 
WHERE created_at >= NOW() - INTERVAL '1 month'
  AND status = 'pending';

-- Verify partition pruning is working
-- Should show only relevant partition(s) in plan
```

### Cleanup (After 24-48 hours)
```sql
-- If everything works correctly, drop old table
-- ONLY after verifying data integrity!
DROP TABLE IF EXISTS transactions_old;

-- Vacuum to reclaim space
VACUUM ANALYZE transactions;
```

## Rollback Procedure (If Needed)

### Emergency Rollback
```sql
-- Stop application first!

-- Drop partitioned table
DROP TABLE IF EXISTS transactions CASCADE;

-- Restore old table
ALTER TABLE transactions_old RENAME TO transactions;

-- Recreate indexes
CREATE INDEX idx_transactions_status ON transactions(status);
CREATE INDEX idx_transactions_stellar_account ON transactions(stellar_account);

-- Restart application
```

### Rollback from Backup
```bash
# If migration failed catastrophically
psql -U synapse -d synapse < backup_before_partitioning_YYYYMMDD.sql
```

## Ongoing Maintenance

### Daily Checks (First Week)
- [ ] Check partition manager logs
- [ ] Verify new records going to correct partitions
- [ ] Monitor query performance
- [ ] Check partition sizes

### Weekly Checks
- [ ] Review partition count (should increase by ~1 per month)
- [ ] Check for detached partitions (after 12 months)
- [ ] Verify maintenance function runs successfully
- [ ] Review storage usage

### Monthly Checks
- [ ] Archive detached partitions
- [ ] Review retention policy (adjust if needed)
- [ ] Analyze query patterns for optimization
- [ ] Update documentation if needed

## Troubleshooting

### Partition Not Created
```sql
-- Manually create partition
SELECT create_monthly_partition();

-- Check for errors
SELECT * FROM pg_stat_activity WHERE query LIKE '%partition%';
```

### Data Not Routing to Partition
```sql
-- Check partition bounds
SELECT 
    c.relname,
    pg_get_expr(c.relpartbound, c.oid)
FROM pg_class c
JOIN pg_inherits i ON c.oid = i.inhrelid
WHERE i.inhparent = 'transactions'::regclass;

-- Verify created_at value is within bounds
```

### Partition Manager Not Running
```bash
# Check application logs
tail -f /var/log/synapse-core.log | grep -i partition

# Verify background task started
# Should see: "Partition manager started"
```

### Performance Issues
```sql
-- Reindex partitions
REINDEX TABLE transactions;

-- Analyze statistics
ANALYZE transactions;

-- Check for bloat
SELECT 
    schemaname,
    tablename,
    pg_size_pretty(pg_total_relation_size(schemaname||'.'||tablename)) AS size
FROM pg_tables
WHERE tablename LIKE 'transactions_y%'
ORDER BY pg_total_relation_size(schemaname||'.'||tablename) DESC;
```

## Success Criteria

- [x] Migration completed without errors
- [x] 3 initial partitions created
- [x] Data migrated successfully
- [x] Partition manager running
- [x] INSERT operations work correctly
- [x] Query performance improved
- [x] Partition pruning verified
- [x] Documentation complete
- [x] Team trained on new system

## Contacts & Resources

- **Documentation**: `docs/partitioning.md`
- **Quick Reference**: `PARTITION_QUICK_REF.md`
- **Architecture**: `docs/partition_architecture.md`
- **SQL Utilities**: `migrations/partition_utils.sql`
- **Issue**: #16 - Database Partitioning for High Volume

## Sign-off

- [ ] Database Administrator approval
- [ ] Backend Team Lead approval
- [ ] DevOps approval
- [ ] Production deployment scheduled
- [ ] Rollback plan reviewed and approved

---

**Deployment Date**: _________________
**Deployed By**: _________________
**Verified By**: _________________
