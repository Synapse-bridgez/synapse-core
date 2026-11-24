# Database Pagination

This document describes the pagination strategy used in the Synapse Core database layer.

## Overview

Synapse Core uses **cursor-based pagination** for efficient, scalable data retrieval. This approach is superior to offset-based pagination for large datasets because it:

- Avoids the O(n) cost of skipping rows
- Remains consistent even when data is inserted/deleted between requests
- Provides stable ordering across concurrent requests

## Cursor-Based Pagination

### How It Works

Cursors are tuples of `(created_at, id)` that uniquely identify a position in the result set:

```rust
pub async fn list_transactions(
    pool: &PgPool,
    limit: i64,
    cursor: Option<(DateTime<Utc>, Uuid)>,
    backward: bool,
) -> Result<Vec<Transaction>>
```

### Parameters

- **limit**: Maximum number of records to return (typically 20-100)
- **cursor**: Optional tuple of `(timestamp, uuid)` marking the starting position
- **backward**: Direction of pagination
  - `false`: Forward (most recent first, then older)
  - `true`: Backward (oldest first, then newer)

### Query Strategy

#### Forward Pagination (Most Recent First)

```sql
SELECT * FROM transactions 
WHERE (created_at, id) < ($1, $2) 
ORDER BY created_at DESC, id DESC 
LIMIT $3
```

This retrieves records **before** the cursor position, ordered newest-first.

#### Backward Pagination (Oldest First)

```sql
SELECT * FROM transactions 
WHERE (created_at, id) > ($1, $2) 
ORDER BY created_at ASC, id ASC 
LIMIT $3
```

This retrieves records **after** the cursor position, ordered oldest-first, then reverses the result set.

### Composite Index

The queries rely on a composite index for performance:

```sql
CREATE INDEX idx_transactions_created_id ON transactions(created_at DESC, id DESC);
```

This index enables efficient range queries on the `(created_at, id)` tuple.

## Usage Examples

### Initial Request (No Cursor)

```rust
// Get the first 20 most recent transactions
let transactions = list_transactions(&pool, 20, None, false).await?;
```

### Subsequent Request (With Cursor)

```rust
// Get the next 20 transactions after the last one
let last_tx = &transactions[transactions.len() - 1];
let cursor = (last_tx.created_at, last_tx.id);
let next_transactions = list_transactions(&pool, 20, Some(cursor), false).await?;
```

### Backward Pagination

```rust
// Get older transactions (reverse direction)
let transactions = list_transactions(&pool, 20, Some(cursor), true).await?;
```

## Filtered Pagination

For filtered queries, use `list_transactions_filtered`:

```rust
pub async fn list_transactions_filtered(
    pool: &PgPool,
    limit: i64,
    cursor: Option<(DateTime<Utc>, Uuid)>,
    backward: bool,
    filters: TransactionFilters,
) -> Result<Vec<Transaction>>
```

This applies the same cursor-based pagination with additional WHERE clauses for filtering.

## Performance Characteristics

| Operation | Complexity | Notes |
|-----------|-----------|-------|
| First page | O(limit) | Single index scan |
| Subsequent pages | O(limit) | Constant time regardless of dataset size |
| Backward pagination | O(limit) | Requires result reversal |

## Security Considerations

1. **Limit Validation**: Always validate and clamp the limit parameter
   ```rust
   let limit = limit.clamp(1, MAX_LIMIT);
   ```

2. **Cursor Validation**: Cursors are opaque to clients; validate format before use

3. **Authorization**: Apply tenant/user filters before pagination

## Best Practices

1. **Use Reasonable Limits**: Typically 20-100 records per page
2. **Cache Cursors**: Store cursors client-side to avoid re-fetching
3. **Handle Empty Results**: Check for empty result sets to detect end of pagination
4. **Consistent Ordering**: Always use the same sort order (created_at DESC, id DESC)
5. **Monitor Query Performance**: Use slow query logs to detect pagination issues

## Partitioning Integration

When using partitioned tables, cursor-based pagination works seamlessly:

- Queries automatically scan relevant partitions
- Composite index spans all partitions
- No special handling required in application code

See [partitioning.md](partitioning.md) for details on table partitioning.

## `first` and `last` are mutually exclusive

A cursor pagination request may set `first` (page forward, usually with `after`) or `last` (page backward, usually with `before`), never both. Sending both is rejected before any query runs with a GraphQL validation error, `extensions.code = "VALIDATION_ERROR"`, whose message names both `first` and `last`. The check lives in `PaginationArgs::validate` (`src/graphql/pagination.rs`); `CursorPagination::new` enforces the same rule for code that builds cursor parameters directly.
