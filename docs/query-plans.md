# Query Plans

## Automatic Slow-Query Plan Capture

When a query exceeds `SLOW_QUERY_THRESHOLD_MS` (default 500 ms), the application
automatically captures an `EXPLAIN (ANALYZE, BUFFERS, FORMAT TEXT)` plan and
persists it to the `slow_query_plans` table for post-hoc analysis.

### How It Works

1. The original query completes normally and returns to the caller.
2. A Tokio task is spawned (fire-and-forget) that re-issues the query wrapped
   in `EXPLAIN (ANALYZE, BUFFERS)` on a separate pool connection.
3. The plan text is stored in `slow_query_plans` with the query name, SQL,
   duration, and threshold.

**Only read queries are re-executed.** Write queries (`QueryKind::Write`) receive
a log entry and metric increment but never trigger plan re-execution, eliminating
any risk of double-writes.

### Configuration

| Env var | Default | Description |
|---|---|---|
| `SLOW_QUERY_THRESHOLD_MS` | `500` | Queries slower than this (ms) get a plan captured |

### Querying Captured Plans

```sql
-- All slow queries in the last 24 hours, slowest first:
SELECT captured_at, query_name, duration_ms, threshold_ms, plan_text
FROM slow_query_plans
WHERE captured_at > NOW() - INTERVAL '24 hours'
ORDER BY duration_ms DESC;

-- Plans for a specific query by name:
SELECT captured_at, duration_ms, plan_text
FROM slow_query_plans
WHERE query_name = 'get_daily_totals'
ORDER BY captured_at DESC
LIMIT 10;

-- Count slow queries per name over the last week:
SELECT query_name, COUNT(*) AS occurrences, AVG(duration_ms) AS avg_ms
FROM slow_query_plans
WHERE captured_at > NOW() - INTERVAL '7 days'
GROUP BY query_name
ORDER BY occurrences DESC;
```

### Plan Capture in Application Code

Use `log_query_timing_with_plan` at call sites that have pool access:

```rust
use synapse_core::db::slow_query::{log_query_timing_with_plan, QueryKind};

// Read query — plan will be captured if slow
log_query_timing_with_plan(
    pool,
    "fetch_transactions",
    "SELECT * FROM transactions WHERE status = $1",
    duration_ms,
    rows_affected,
    config.slow_query_threshold_ms,
    QueryKind::Read,
);

// Write query — plan capture skipped, no re-execution
log_query_timing_with_plan(
    pool,
    "update_status",
    "UPDATE transactions SET status = $1 WHERE id = $2",
    duration_ms,
    rows_affected,
    config.slow_query_threshold_ms,
    QueryKind::Write,
);
```

### Storage Retention

Plans are pruned automatically via `prune_old_plans(pool, Some(30))` (default 30
days).  Wire this into the cron scheduler to keep the table bounded.  Individual
plans are truncated at 64 KiB to prevent runaway storage from very large plans.

### Running the Test

```bash
# Requires Docker
cargo test --lib slow_query -- --ignored --nocapture
```

---

## Transaction Search Query Plans

## Indexes Added (migration `20260427000000_optimized_search_indexes`)

| Index | Type | Columns | Purpose |
|---|---|---|---|
| `idx_transactions_status_asset_created` | B-tree | `(status, asset_code, created_at DESC)` | Multi-filter search + sort |
| `idx_transactions_pending` | Partial B-tree | `(created_at DESC) WHERE status = 'pending'` | Processor queue scans |
| `idx_transactions_metadata_gin` | GIN (`jsonb_path_ops`) | `metadata` | JSON path queries |

## Common Query Patterns

### 1. Search by status + asset_code (most common)

```sql
EXPLAIN ANALYZE
SELECT * FROM transactions
WHERE status = 'completed' AND asset_code = 'USDC'
ORDER BY created_at DESC, id DESC
LIMIT 50;
```

Expected plan (>100K rows):
```
Index Scan using idx_transactions_status_asset_created on transactions
  Index Cond: ((status = 'completed') AND (asset_code = 'USDC'))
  ...
  Rows Removed by Filter: ~0
Planning Time: ~0.3 ms
Execution Time: ~1.2 ms   (vs ~180 ms seq scan)
```

### 2. Processor pending queue

```sql
EXPLAIN ANALYZE
SELECT * FROM transactions
WHERE status = 'pending'
ORDER BY created_at DESC
LIMIT 100;
```

Expected plan:
```
Index Scan using idx_transactions_pending on transactions
  (partial index, only scans pending rows)
Execution Time: ~0.8 ms
```

### 3. Metadata JSON path query

```sql
EXPLAIN ANALYZE
SELECT * FROM transactions
WHERE metadata @? '$.source_bank_id ? (@ == "CHASE")';
```

Expected plan:
```
Bitmap Index Scan on idx_transactions_metadata_gin
  Index Cond: (metadata @? ...)
```

## Performance Improvement

On a table with 100K+ rows, the composite index eliminates sequential scans
for the two most common search filter combinations (`status` alone,
`status + asset_code`), reducing query time by >50% in benchmarks.
