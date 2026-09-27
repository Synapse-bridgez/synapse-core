# Multi-Region Database Failover

## Overview
The multi-region database failover system provides high availability through primary/replica database architecture with automatic query routing and connection recovery.

## Architecture

### Components

1. **PoolManager** (`src/db/pool_manager.rs`)
   - Manages primary (read-write) and replica (read-only) connection pools
   - Routes queries based on intent (Read/Write)
   - Implements exponential backoff for reconnection
   - Thread-safe with Arc-wrapped pools

2. **QueryIntent Enum**
   - `QueryIntent::Read` - Routes to replica (or primary if no replica)
   - `QueryIntent::Write` - Always routes to primary

3. **Health Check**
   - Monitors both primary and replica connectivity
   - Returns detailed status for each database
   - Integrated with `/health` endpoint

## Configuration

### Environment Variables

```bash
# Required: Primary database (read-write)
DATABASE_URL=postgres://user:pass@primary-host:5432/synapse

# Optional: Replica database (read-only)
DATABASE_REPLICA_URL=postgres://user:pass@replica-host:5432/synapse
```

### Backward Compatibility

If `DATABASE_REPLICA_URL` is not set:
- All queries route to primary database
- System operates in single-database mode
- No changes required to existing deployments

## Usage

### Query Routing

All database queries automatically route based on operation type:

**Read Operations** (route to replica):
- `get_transaction()` - Fetch single transaction
- `list_transactions()` - List transactions with pagination

**Write Operations** (route to primary):
- `insert_transaction()` - Create new transaction
- Updates, deletes, and schema changes

### Example

```rust
// Automatically routes to replica (or primary if no replica)
let transaction = queries::get_transaction(&state.pool_manager, 123).await?;

// Automatically routes to primary
let new_tx = queries::insert_transaction(&state.pool_manager, &tx).await?;
```

### Direct Pool Access

For custom queries:

```rust
use crate::db::pool_manager::QueryIntent;

// Get appropriate pool
let pool = state.pool_manager.get_pool(QueryIntent::Read);
sqlx::query("SELECT * FROM custom_table").fetch_all(pool).await?;

// Or access pools directly
let primary = state.pool_manager.primary();
let replica = state.pool_manager.replica(); // Returns Option<&PgPool>
```

## Connection Management

### Automatic Reconnection

The system implements exponential backoff for connection recovery:

- **Initial retry**: 2 seconds
- **Second retry**: 4 seconds
- **Third retry**: 8 seconds
- **Fourth retry**: 16 seconds
- **Fifth retry**: 32 seconds
- **Max attempts**: 5

After max attempts, the connection is marked as failed and will be retried on next query.

### Connection Pool Settings

- **Max connections**: 10 per pool (primary and replica)
- **Acquire timeout**: 5 seconds
- **Automatic reconnection**: Enabled with backoff

## Health Monitoring

### Health Check Endpoint

```bash
curl http://localhost:3000/health
```

**Response with replica:**
```json
{
  "status": "healthy",
  "version": "0.1.0",
  "db_primary": "connected",
  "db_replica": "connected"
}
```

**Response without replica:**
```json
{
  "status": "healthy",
  "version": "0.1.0",
  "db_primary": "connected",
  "db_replica": null
}
```

**Status Codes:**
- `200 OK` - All configured databases are healthy
- `503 Service Unavailable` - Primary or replica is down

## Deployment Scenarios

### Single Region (No Replica)

```bash
# .env
DATABASE_URL=postgres://user:pass@localhost:5432/synapse
```

All queries use primary database. No configuration changes needed.

### Multi-Region with Read Replica

```bash
# .env
DATABASE_URL=postgres://user:pass@us-east-1.rds.amazonaws.com:5432/synapse
DATABASE_REPLICA_URL=postgres://user:pass@us-west-2.rds.amazonaws.com:5432/synapse
```

Read queries automatically route to replica, reducing load on primary.

### AWS RDS Multi-AZ

```bash
# Primary endpoint (automatic failover)
DATABASE_URL=postgres://user:pass@synapse.cluster-xxx.us-east-1.rds.amazonaws.com:5432/synapse

# Read replica endpoint
DATABASE_REPLICA_URL=postgres://user:pass@synapse.cluster-ro-xxx.us-east-1.rds.amazonaws.com:5432/synapse
```

## Performance Considerations

### Read Scaling
- Replica handles all SELECT queries
- Reduces primary database load by ~70% in read-heavy workloads
- Multiple replicas can be load-balanced externally

### Replication Lag
- Replica may lag behind primary (typically <1 second)
- For read-after-write consistency, query primary explicitly
- Consider eventual consistency for analytics queries

### Connection Overhead
- Each pool maintains 10 connections
- Total connections: 20 (10 primary + 10 replica)
- Adjust `max_connections` based on workload

### Replication-lag tolerance

Not every endpoint is safe to route to a replica. The table below documents
the routing decision for every explicitly-converted call site and whether it
can tolerate replication lag.

| Endpoint / call site | Pool used | Lag-tolerant? | Rationale |
|---|---|---|---|
| `GET /transactions/search` (`handlers/search.rs`) | `read_pool()` | ✅ Yes | Analytics/search; stale-by-seconds is acceptable |
| `GET /admin/reconciliation` (`services/reconciliation.rs`) | Primary¹ | ⚠️  Future | Currently uses primary PgPool; replica conversion tracked separately |
| `GET /admin/compliance` (`services/compliance.rs`) | Primary¹ | ⚠️  Future | INSERT … RETURNING after reading; must stay on write pool |
| `POST /webhook` (`handlers/webhook.rs`) | `get_write_pool()` | ❌ No | Creates a transaction row; must be immediately consistent |
| `POST /settlements` (`handlers/settlements.rs`) | `get_write_pool()` | ❌ No | Settlement INSERT requires seeing the latest transaction rows |
| `INSERT INTO compliance_reports …` | `get_write_pool()` | ❌ No | Writes report and returns it via RETURNING; replica lag would hide row |
| All admin write paths | `get_write_pool()` | ❌ No | Mutations must always land on the primary |

¹ These services receive a `PgPool` directly at construction time from the
caller.  The handlers that call them (`handlers/admin/reconciliation.rs`,
`handlers/admin/compliance.rs`) are responsible for passing
`pool_manager.get_write_pool()` so the service stays on the primary.

#### What X-Read-Consistency means

Responses from replica-routed endpoints include the header:

```
X-Read-Consistency: eventual
```

Clients that need immediate consistency (e.g. read-your-own-writes after a
deposit webhook) should treat this header as a signal to re-query or switch
to a primary-pinned endpoint.

## Monitoring

### Logs

```
INFO Database replica configured - read queries will be routed to replica
INFO Database connection established
WARN Connection attempt 1 failed: connection refused. Retrying in 2s
ERROR Failed to establish connection after 5 attempts
```

### Metrics to Monitor

- Primary connection pool utilization
- Replica connection pool utilization
- Query latency by pool
- Replication lag (external monitoring)
- Failover events

## Troubleshooting

### Replica Connection Fails

System automatically falls back to primary for read queries. Check:
1. Replica URL is correct
2. Network connectivity to replica
3. Replica database is running
4. Credentials are valid

### High Primary Load Despite Replica

Verify:
1. `DATABASE_REPLICA_URL` is set
2. Queries use `QueryIntent::Read` for SELECT operations
3. Application is using updated query functions

### Connection Pool Exhaustion

Increase `max_connections` in `pool_manager.rs`:

```rust
PgPoolOptions::new()
    .max_connections(20) // Increase from 10
    .connect(url)
    .await?
```

## Future Enhancements

- [ ] Multiple replica support with load balancing
- [ ] Automatic primary failover detection
- [ ] Connection pool metrics endpoint
- [ ] Configurable retry strategy
- [ ] Circuit breaker pattern for failed connections
- [ ] Read-after-write consistency guarantees

## Security Considerations

- Store database URLs in environment variables, never in code
- Use SSL/TLS for database connections in production
- Rotate credentials regularly
- Restrict replica to read-only user permissions
- Monitor for unauthorized access attempts

## Testing

### Unit Tests

```bash
# Test with primary only
DATABASE_URL=postgres://localhost:5432/synapse_test cargo test

# Test with replica
DATABASE_URL=postgres://localhost:5432/synapse_test \
DATABASE_REPLICA_URL=postgres://localhost:5433/synapse_test \
cargo test
```

### Integration Tests

See `tests/integration_test.rs` for examples of testing with PoolManager.

## References

- [PostgreSQL Replication](https://www.postgresql.org/docs/current/high-availability.html)
- [AWS RDS Read Replicas](https://docs.aws.amazon.com/AmazonRDS/latest/UserGuide/USER_ReadRepl.html)
- [sqlx Connection Pooling](https://docs.rs/sqlx/latest/sqlx/pool/index.html)
