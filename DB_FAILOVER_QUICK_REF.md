# Database Failover - Quick Reference

## Setup (2 minutes)

### 1. Add Replica URL
```bash
echo "DATABASE_REPLICA_URL=postgres://user:pass@replica-host:5432/synapse" >> .env
```

### 2. Restart Service
```bash
cargo run
```

### 3. Verify
```bash
curl http://localhost:3000/health
```

Expected output:
```json
{
  "status": "healthy",
  "db_primary": "connected",
  "db_replica": "connected"
}
```

## Query Routing

| Operation | Routes To | Function |
|-----------|-----------|----------|
| SELECT | Replica | `get_transaction()`, `list_transactions()` |
| INSERT | Primary | `insert_transaction()` |
| UPDATE | Primary | (future) |
| DELETE | Primary | (future) |

## Configuration

```bash
# Minimum (current setup)
DATABASE_URL=postgres://localhost:5432/synapse

# With replica (high availability)
DATABASE_URL=postgres://primary:5432/synapse
DATABASE_REPLICA_URL=postgres://replica:5432/synapse
```

## Connection Settings

- **Max connections per pool**: 10
- **Acquire timeout**: 5 seconds
- **Retry attempts**: 5
- **Backoff**: Exponential (2s, 4s, 8s, 16s, 32s)

## Health Check

```bash
# Check status
curl http://localhost:3000/health | jq

# Expected responses
# Both healthy: 200 OK
# Any down: 503 Service Unavailable
```

## Troubleshooting

### Replica not connecting
```bash
# Check logs
grep "replica" logs/app.log

# Test connection manually
psql $DATABASE_REPLICA_URL -c "SELECT 1"
```

### High primary load
```bash
# Verify replica is configured
curl http://localhost:3000/health | jq .db_replica

# Should show "connected", not null
```

### Disable replica
```bash
# Remove from .env
sed -i '/DATABASE_REPLICA_URL/d' .env

# Restart
cargo run
```

## AWS RDS Setup

```bash
# 1. Create read replica in RDS console
# 2. Get endpoint from RDS console
# 3. Add to .env

DATABASE_URL=postgres://user:pass@synapse.xxx.us-east-1.rds.amazonaws.com:5432/synapse
DATABASE_REPLICA_URL=postgres://user:pass@synapse-ro.xxx.us-east-1.rds.amazonaws.com:5432/synapse
```

## Performance

| Metric | Single DB | With Replica |
|--------|-----------|--------------|
| Primary Load | 100% | ~30% |
| Read Latency | Same | Potentially lower |
| Write Latency | Same | Same |
| Connections | 5 | 20 |

## Code Examples

### Using PoolManager
```rust
use crate::db::pool_manager::QueryIntent;

// Read query (uses replica)
let pool = state.pool_manager.get_pool(QueryIntent::Read);
let result = sqlx::query("SELECT * FROM transactions")
    .fetch_all(pool)
    .await?;

// Write query (uses primary)
let pool = state.pool_manager.get_pool(QueryIntent::Write);
sqlx::query("INSERT INTO transactions ...")
    .execute(pool)
    .await?;
```

### Health Check
```rust
let health = state.pool_manager.health_check().await;
println!("Primary: {}", health.primary);
println!("Replica: {}", health.replica);
```

## Testing

```bash
# Unit tests
cargo test db_failover

# With replica
DATABASE_REPLICA_URL=postgres://localhost:5433/synapse_test cargo test

# Integration tests
cargo test --test db_failover_test
```

## Monitoring Commands

```bash
# Watch health status
watch -n 5 'curl -s http://localhost:3000/health | jq'

# Monitor logs
tail -f logs/app.log | grep -E "(replica|connection|pool)"

# Check PostgreSQL replication lag
psql $DATABASE_URL -c "SELECT * FROM pg_stat_replication;"
```

## Common Issues

| Issue | Solution |
|-------|----------|
| Replica shows null | `DATABASE_REPLICA_URL` not set |
| 503 errors | Check database connectivity |
| High latency | Check replication lag |
| Connection timeout | Increase `acquire_timeout` |

## Rollback

```bash
# Remove replica configuration
unset DATABASE_REPLICA_URL

# Or comment in .env
# DATABASE_REPLICA_URL=...

# Restart service
cargo run
```

System automatically falls back to primary-only mode.

## Documentation

- Full docs: `docs/database_failover.md`
- PR description: `DB_FAILOVER_PR.md`
- Tests: `tests/db_failover_test.rs`
