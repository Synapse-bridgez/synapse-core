# Database Failover Deployment Checklist

## Pre-Deployment

### Code Review
- [ ] Review all code changes in PR
- [ ] Verify backward compatibility
- [ ] Check test coverage
- [ ] Review documentation completeness
- [ ] Validate security considerations

### Testing
- [ ] Run unit tests: `cargo test`
- [ ] Run integration tests: `cargo test db_failover`
- [ ] Test with primary only configuration
- [ ] Test with primary + replica configuration
- [ ] Verify health endpoint responses
- [ ] Test connection recovery (simulate network issues)

### Infrastructure
- [ ] PostgreSQL replica is set up and running
- [ ] Replication is working (check `pg_stat_replication`)
- [ ] Replica is in sync with primary
- [ ] Network connectivity between app and replica verified
- [ ] Firewall rules allow connections to replica
- [ ] SSL/TLS certificates configured (if required)

### Configuration
- [ ] `DATABASE_REPLICA_URL` prepared for production
- [ ] Credentials for replica database created
- [ ] Read-only user configured on replica (recommended)
- [ ] Connection limits reviewed and adjusted
- [ ] Environment variables documented

## Deployment Steps

### 1. Merge Code
```bash
git checkout develop
git merge feature/issue-30-db-failover
git push origin develop
```

### 2. Build and Test
```bash
cargo build --release
cargo test --release
```

### 3. Deploy to Staging
```bash
# Update .env with replica URL
echo "DATABASE_REPLICA_URL=postgres://user:pass@staging-replica:5432/synapse" >> .env

# Deploy
./deploy-staging.sh

# Verify
curl https://staging.synapse.com/health
```

### 4. Staging Validation
- [ ] Health check shows both databases connected
- [ ] Application logs show "Database replica configured"
- [ ] Read queries are hitting replica (check PostgreSQL logs)
- [ ] Write queries are hitting primary
- [ ] No errors in application logs
- [ ] Performance metrics look good

### 5. Deploy to Production
```bash
# Update production .env
DATABASE_REPLICA_URL=postgres://user:pass@prod-replica:5432/synapse

# Deploy with zero downtime
./deploy-production.sh

# Verify immediately
curl https://api.synapse.com/health
```

## Post-Deployment

### Immediate Checks (First 5 minutes)
- [ ] Health endpoint returns 200 OK
- [ ] Both `db_primary` and `db_replica` show "connected"
- [ ] No errors in application logs
- [ ] Application is responding to requests
- [ ] Response times are normal or improved

### Short-term Monitoring (First Hour)
- [ ] Monitor primary database connections (should be ~30% of previous)
- [ ] Monitor replica database connections (should be ~70% of previous)
- [ ] Check replication lag (should be <1 second)
- [ ] Verify no connection pool exhaustion
- [ ] Monitor error rates
- [ ] Check response time percentiles (p50, p95, p99)

### Long-term Monitoring (First 24 Hours)
- [ ] Database CPU usage (primary should be lower)
- [ ] Database memory usage
- [ ] Connection pool utilization
- [ ] Query latency by operation type
- [ ] Replication lag trends
- [ ] Error rates and types

## Rollback Plan

### If Issues Occur

#### Quick Rollback (Remove Replica)
```bash
# 1. Remove replica URL from .env
sed -i '/DATABASE_REPLICA_URL/d' .env

# 2. Restart application
systemctl restart synapse-core

# 3. Verify
curl http://localhost:3000/health
# Should show db_replica: null
```

#### Full Rollback (Previous Version)
```bash
# 1. Deploy previous version
git checkout <previous-commit>
./deploy-production.sh

# 2. Verify
curl https://api.synapse.com/health
```

## Monitoring Queries

### Check Replication Status
```sql
-- On primary database
SELECT * FROM pg_stat_replication;
```

### Check Connection Counts
```sql
-- On primary
SELECT count(*) FROM pg_stat_activity WHERE datname = 'synapse';

-- On replica
SELECT count(*) FROM pg_stat_activity WHERE datname = 'synapse';
```

### Check Replication Lag
```sql
-- On replica
SELECT now() - pg_last_xact_replay_timestamp() AS replication_lag;
```

## Success Criteria

### Deployment is Successful If:
- [x] Health endpoint returns 200 OK
- [x] Both databases show "connected"
- [x] No increase in error rates
- [x] Response times stable or improved
- [x] Primary database load reduced by ~50-70%
- [x] Replication lag < 1 second
- [x] No connection pool exhaustion

### Deployment Should Be Rolled Back If:
- [ ] Health endpoint returns 503
- [ ] Error rate increases by >5%
- [ ] Response time increases by >20%
- [ ] Connection pool exhaustion occurs
- [ ] Replication lag > 5 seconds consistently
- [ ] Any database becomes unreachable

## Communication

### Before Deployment
- [ ] Notify team of deployment window
- [ ] Prepare rollback plan
- [ ] Ensure on-call engineer is available

### During Deployment
- [ ] Post status updates in team channel
- [ ] Monitor metrics dashboard
- [ ] Be ready to rollback if needed

### After Deployment
- [ ] Confirm successful deployment
- [ ] Share metrics showing improvement
- [ ] Document any issues encountered
- [ ] Update runbook if needed

## Documentation Updates

### Post-Deployment
- [ ] Update production runbook
- [ ] Document replica endpoint
- [ ] Update monitoring dashboards
- [ ] Add alerts for replication lag
- [ ] Update incident response procedures

## Metrics to Track

### Before Deployment (Baseline)
- Primary database CPU: ____%
- Primary database connections: ____
- Average response time: ____ms
- P95 response time: ____ms
- Error rate: ____%

### After Deployment (Target)
- Primary database CPU: ____% (expect 30-50% of baseline)
- Primary database connections: ____ (expect ~30% of baseline)
- Replica database connections: ____ (expect ~70% of baseline)
- Average response time: ____ms (expect same or better)
- P95 response time: ____ms (expect same or better)
- Error rate: ____% (expect same)
- Replication lag: ____ms (expect <1000ms)

## Sign-off

- [ ] Code reviewed by: ________________
- [ ] Tests verified by: ________________
- [ ] Infrastructure ready: ________________
- [ ] Deployment approved by: ________________
- [ ] Deployed by: ________________
- [ ] Verified by: ________________

## Notes

Date: ________________
Deployment time: ________________
Issues encountered: ________________
Resolution: ________________

---

**Status**: Ready for deployment
**Risk Level**: Low (fully backward compatible)
**Estimated Downtime**: Zero
**Rollback Time**: <5 minutes
