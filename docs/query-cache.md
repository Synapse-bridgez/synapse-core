# Query Result Caching Layer

## Overview

The query result caching layer provides sophisticated caching for expensive aggregate queries with automatic cache invalidation based on data changes. This reduces database load for frequently accessed dashboard statistics and analytics.

## Features

✅ **Cache aggregate query results** - Status counts, daily totals, asset statistics  
✅ **Automatic cache invalidation** - Transactional invalidation on data changes  
✅ **Cache warming on startup** - Pre-populate cache with common queries  
✅ **Cache hit/miss metrics** - Exposed via `/cache/metrics` endpoint  
✅ **Configurable TTL per query type** - Different expiration times for different queries  
✅ **Tenant-scoped keys** - Cache entries are isolated per tenant

## Architecture

### Input validation

All `QueryCache` Redis operations validate keys, patterns, values, and TTLs via [`CacheValidator`](../src/cache/validation.rs) before network I/O. See [Cache Input Validation](./cache-input-validation.md) for rules, security rationale, and troubleshooting.

### Components

1. **QueryCache Service** (`src/services/query_cache.rs`)
   - Redis-backed caching layer
   - Generic get/set operations with TTL support
   - Pattern-based and exact key invalidation
   - Atomic hit/miss metrics tracking

2. **Cache Key Generation**

   Every key is prefixed with the tenant id so that tenants never share cached
   results. The tenant id is taken from the request context (see
   `src/middleware/tenant.rs`).

   - `cache_key_status_counts(tenant)` → `"query:{tenant}:status_counts"`
   - `cache_key_daily_totals(tenant, days)` → `"query:{tenant}:daily_totals:{days}"`
   - `cache_key_asset_stats(tenant)` → `"query:{tenant}:asset_stats"`
   - `cache_key_asset_total(tenant, asset)` → `"query:{tenant}:asset_total:{asset}"`

3. **Invalidation Hooks**
   - `insert_transaction()` - Invalidates after transaction insert
   - `update_transactions_settlement()` - Invalidates after settlement
   - `settle_asset()` - Invalidates after settlement commit
   - `process_transaction()` - Invalidates after status update
   - `requeue_dlq()` - Invalidates after DLQ requeue
   - `force_complete_transaction()` - Invalidates in GraphQL/CLI

## Configuration

### Cache TTL Settings

```rust
CacheConfig {
    status_counts_ttl: 300,    // 5 minutes
    daily_totals_ttl: 3600,    // 1 hour
    asset_stats_ttl: 600,      // 10 minutes
}
```

Customize by modifying `CacheConfig::default()` in `src/services/query_cache.rs`.

### Environment Variables

```bash
REDIS_URL=redis://localhost:6379
```

## Usage

### Querying with Cache

The cache is automatically used by stats endpoints:

```bash
# Status counts (cached for 5 minutes)
GET /stats/status

# Daily totals (cached for 1 hour)
GET /stats/daily?days=7

# Asset statistics (cached for 10 minutes)
GET /stats/assets
```

### Cache Metrics

Monitor cache performance:

```bash
GET /cache/metrics
```

Response:
```json
{
  "hits": 1250,
  "misses": 48,
  "total": 1298,
  "hit_rate": 96.3
}
```

### Manual Cache Warming

Cache warming happens automatically on startup. To manually trigger:

```rust
let cache_config = CacheConfig::default();
query_cache.warm_cache(&pool, &cache_config).await?;
```

## Cache Invalidation Strategy

### Transactional Invalidation

Cache invalidation is **always performed after successful database commits** to ensure consistency:

```rust
// Example from insert_transaction
db_tx.commit().await?;  // Commit first
invalidate_transaction_caches(&asset_code).await;  // Then invalidate
```

### Invalidation Patterns

Invalidation uses the same tenant-scoped key shape as reads, so a write for a
given tenant only clears that tenant's entries:

1. **Status counts** - `query:{tenant}:status_counts`
2. **Daily totals** - `query:{tenant}:daily_totals:*` (all day ranges)
3. **Asset stats** - `query:{tenant}:asset_stats`
4. **Asset total** - `query:{tenant}:asset_total:{asset_code}` (specific asset)

### Write Operations with Invalidation

| Operation | Location | Invalidates |
|-----------|----------|-------------|
| Insert transaction | `db/queries.rs::insert_transaction` | All query caches for asset |
| Update settlement | `db/queries.rs::update_transactions_settlement` | Via settlement service |
| Settle asset | `services/settlement.rs::settle_asset` | All query caches for asset |
| Process transaction | `services/transaction_processor.rs::process_transaction` | All query caches for asset |
| Requeue DLQ | `services/transaction_processor.rs::requeue_dlq` | All query caches for asset |
| Force complete (GraphQL) | `graphql/resolvers/transaction.rs` | All query caches for asset |
| Force complete (CLI) | `cli.rs::handle_tx_force_complete` | All query caches for asset |
| Batch processor | `services/processor.rs::process_batch` | All affected assets |

## Implementation Details

### Cache Flow

```
┌─────────────┐
│   Request   │
└──────┬──────┘
       │
       ▼
┌─────────────────┐
│  Check Cache    │◄─── Hit: Return cached result
└──────┬──────────┘
       │ Miss
       ▼
┌─────────────────┐
│  Query Database │
└──────┬──────────┘
       │
       ▼
┌─────────────────┐
│  Store in Cache │
└──────┬──────────┘
       │
       ▼
┌─────────────────┐
│  Return Result  │
└─────────────────┘
```

### Invalidation Flow

```
┌──────────────────┐
│  Write Operation │
└────────┬─────────┘
         │
         ▼
┌──────────────────┐
│  Begin TX        │
└────────┬─────────┘
         │
         ▼
┌──────────────────┐
│  Execute Query   │
└────────┬─────────┘
         │
         ▼
┌──────────────────┐
│  Commit TX       │
└────────┬─────────┘
         │
         ▼
┌──────────────────┐
│  Invalidate      │
│  Cache Keys      │
└──────────────────┘
```

## Testing

### Unit Tests

```bash
cargo test query_cache
```

Tests include:
- Basic get/set operations
- Cache miss handling
- Metrics tracking
- Pattern-based invalidation
- Configuration defaults
- Tenant isolation (identical queries for different tenants get distinct entries)

### Integration Tests

Cache is automatically initialized in all integration tests with Redis connection.

## Performance Considerations

### Cache Hit Rate

Target: **>90%** for production workloads

Monitor via `/cache/metrics` endpoint. Low hit rates may indicate:
- TTL too short
- High write volume causing frequent invalidations
- Cache warming not covering common queries

### Memory Usage

Redis memory usage scales with:
- Number of unique query patterns
- Number of tenants
- Size of result sets
- TTL configuration

Typical memory per cached query: **1-10 KB**

### Invalidation Overhead

Invalidation is **non-blocking** and uses fire-and-forget pattern:
```rust
let _ = cache.invalidate("pattern").await;
```

Failed invalidations are logged but don't block the write operation.

## Troubleshooting

### Cache Not Working

1. Check Redis connection:
   ```bash
   redis-cli ping
   ```

2. Verify `REDIS_URL` environment variable

3. Check logs for cache initialization:
   ```
   Query cache initialized
   Cache warming completed
   ```

### High Cache Miss Rate

1. Check TTL configuration - may be too short
2. Verify cache warming is running on startup
3. Check for high write volume causing frequent invalidations

### Stale Data

If seeing stale cached data:
1. Verify all write operations have invalidation hooks
2. Check that invalidation happens **after** commit
3. Manually invalidate: `cache.invalidate("query:*").await`

## Future Enhancements

- [ ] Cache compression for large result sets
- [ ] Distributed cache invalidation for multi-instance deployments
- [ ] Cache preloading based on access patterns
- [ ] Cache stampede prevention with locking

## References

- Issue: #170 Database Query Result Caching Layer
- Issue: #1646 Tenant-scoped query cache keys
- Dependencies: #3 (Database Setup), #64 (Redis Integration)
- Implementation: `src/services/query_cache.rs`
- Tests: `tests/query_cache_test.rs`
