# ADR-001: Database Partitioning Strategy

## Status

Accepted

## Context

Synapse Core processes high-volume transaction data from multiple Stellar Anchor Platform integrations. As the system scales, we anticipate:

- **Millions of transactions per month** from multiple tenants
- **Long-term data retention** requirements (12+ months)
- **Performance degradation** on large table scans
- **Slow query performance** for time-range queries
- **Maintenance challenges** (VACUUM, index rebuilds) on large tables
- **Backup and restore** complexity with multi-GB tables

Without partitioning, the `transactions` table would grow unbounded, leading to:
- Slower queries as table size increases
- Longer VACUUM times blocking operations
- Difficulty archiving old data
- Index bloat and maintenance overhead

## Decision

We will implement **native PostgreSQL table partitioning** using a **monthly time-based partitioning strategy** on the `transactions` table, partitioned by the `created_at` column.

**Key implementation details:**

1. **Partition by month** - Each partition contains one month of data
2. **Automatic partition creation** - Background task creates partitions 2 months in advance
3. **Retention policy** - Detach partitions older than 12 months
4. **Index inheritance** - All indexes automatically created on child partitions
5. **Partition pruning** - PostgreSQL automatically excludes irrelevant partitions from queries

**Partition naming convention:**
```
transactions_y{YYYY}m{MM}
Example: transactions_y2025m02 (February 2025)
```

**Maintenance schedule:**
- Daily background task runs `maintain_partitions()`
- Creates future partitions if missing
- Detaches old partitions based on retention policy

## Consequences

### Positive

- **Query performance improvement** - 70-97% reduction in rows scanned for time-range queries
- **Faster maintenance** - VACUUM and ANALYZE run on smaller partitions
- **Easier archival** - Detach old partitions and export to cold storage
- **Predictable growth** - Each partition has bounded size
- **Better monitoring** - Per-partition metrics for capacity planning
- **Improved backup/restore** - Can backup/restore individual partitions
- **Reduced index bloat** - Smaller indexes per partition

### Negative

- **Increased complexity** - More database objects to manage
- **Partition key constraint** - All queries should include `created_at` for optimal performance
- **Maintenance overhead** - Background task required for partition management
- **Migration complexity** - Existing data must be migrated to partitioned table
- **Cross-partition queries** - Queries spanning many partitions may be slower

### Neutral

- **Storage overhead** - Minimal (metadata for partition definitions)
- **Application changes** - Transparent to application code (queries unchanged)
- **Monitoring requirements** - Need to monitor partition count and sizes

## Alternatives Considered

### Alternative 1: No Partitioning

**Description:** Keep transactions in a single table with optimized indexes.

**Pros:**
- Simpler architecture
- No partition management overhead
- Easier to understand

**Cons:**
- Poor performance at scale (millions of rows)
- Slow VACUUM and maintenance operations
- Difficult to archive old data
- Index bloat over time

**Why rejected:** Does not scale to anticipated transaction volumes. Performance degrades linearly with table size.

### Alternative 2: Application-Level Sharding

**Description:** Create separate tables per tenant or time period, managed by application code.

**Pros:**
- Full control over data distribution
- Can shard by multiple dimensions (tenant + time)

**Cons:**
- Complex application logic
- Difficult to query across shards
- Schema migration complexity
- Connection pool management per shard

**Why rejected:** Adds significant application complexity. Native partitioning provides similar benefits with less code.

### Alternative 3: Time-Series Database (TimescaleDB)

**Description:** Use TimescaleDB extension for PostgreSQL, which provides automatic partitioning (hypertables).

**Pros:**
- Automatic partition management
- Optimized for time-series data
- Built-in compression and retention policies

**Cons:**
- Additional dependency (extension)
- Learning curve for team
- Potential compatibility issues with managed PostgreSQL services
- Overkill for our use case

**Why rejected:** Native partitioning provides sufficient functionality without additional dependencies. TimescaleDB is better suited for high-frequency time-series data (metrics, logs) rather than transactional data.

### Alternative 4: List Partitioning by Tenant

**Description:** Partition by `tenant_id` instead of time.

**Pros:**
- Perfect tenant isolation at storage level
- Easy to backup/restore per tenant
- Can move tenant data to separate database

**Cons:**
- Uneven partition sizes (some tenants much larger)
- Difficult to archive old data
- Requires knowing tenant_id for all queries
- Partition count grows with tenant count

**Why rejected:** Time-based partitioning is more predictable and aligns with our archival requirements. Tenant isolation is achieved through application-level filtering and Row Level Security.

### Alternative 5: Hybrid Partitioning (Tenant + Time)

**Description:** Partition by tenant first, then sub-partition by time.

**Pros:**
- Best of both worlds (tenant isolation + time-based archival)
- Optimal query performance

**Cons:**
- Very complex to manage
- Exponential growth in partition count (tenants × months)
- Difficult to maintain
- Overkill for current scale

**Why rejected:** Premature optimization. We can revisit if we reach hundreds of tenants with millions of transactions each.

## Implementation Notes

### Migration Path

1. **Create partitioned table structure** (Migration 20250217000000)
   - Create parent table with partitioning
   - Create initial partitions
   - Migrate existing data

2. **Deploy partition management functions**
   - `create_monthly_partition()` - Creates next month's partition
   - `detach_old_partitions(retention_months)` - Detaches old partitions
   - `maintain_partitions()` - Runs both functions

3. **Deploy background task**
   - Runs daily at midnight UTC
   - Logs partition creation/detachment events

### Query Optimization

For optimal partition pruning, queries should include explicit time bounds:

**Good (enables partition pruning):**
```sql
SELECT * FROM transactions
WHERE created_at >= '2025-02-01' AND created_at < '2025-03-01'
  AND status = 'completed';
```

**Suboptimal (may scan multiple partitions):**
```sql
SELECT * FROM transactions
WHERE created_at >= NOW() - INTERVAL '30 days'
  AND status = 'completed';
```

**Solution:** Application should compute explicit timestamps:
```rust
let start = Utc::now() - Duration::days(30);
let end = Utc::now();
// Pass start and end as parameters
```

### Monitoring

Monitor these metrics:

- **Partition count** - Should grow by 1 per month
- **Partition sizes** - Should be relatively uniform
- **Query performance** - Check EXPLAIN ANALYZE for partition pruning
- **Maintenance duration** - Time to create/detach partitions
- **Detached partition count** - Ensure old partitions are archived

### Archival Process

When partitions are detached (after 12 months):

1. Partition becomes a standalone table (e.g., `transactions_y2024m01`)
2. Export to CSV or Parquet for cold storage
3. Verify backup integrity
4. Drop the detached table
5. Store archive in S3 or equivalent

## References

- [PostgreSQL Partitioning Documentation](https://www.postgresql.org/docs/current/ddl-partitioning.html)
- [Partition Pruning](https://www.postgresql.org/docs/current/ddl-partitioning.html#DDL-PARTITION-PRUNING)
- [docs/partitioning.md](../partitioning.md) - Detailed implementation guide
- [docs/partition_architecture.md](../partition_architecture.md) - Architecture diagrams
- [Migration 20250217000000_partition_transactions.sql](../../migrations/20250217000000_partition_transactions.sql)
- Issue #XX - Initial partitioning implementation
