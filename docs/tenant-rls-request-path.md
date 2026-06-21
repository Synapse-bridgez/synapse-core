# Tenant RLS Isolation on the Request Path

## Problem

Multi-tenant Row-Level Security (RLS) infrastructure existed but was not wired into production requests:
- `queries::set_tenant_context` was only called from tests
- RLS was effectively untested in live code paths
- **Critical security issue**: `set_tenant_context` used session-scoped GUCs (`set_config(..., false)`), which persist on pooled connections

### Connection Pooling Leak Vector

With session-scoped GUCs:
```
1. Request A from tenant_id=X acquires connection C
   → set_config('app.tenant_id', 'X', false)  ← false = session-scoped
2. Request A completes, returns C to pool
3. Request B from tenant_id=Y acquires the same connection C
   → GUC still contains 'X' from previous request!
   → RLS policy sees 'X' and tenant B sees tenant X's data ← LEAK
```

## Solution: Transaction-Scoped GUCs with SET LOCAL

Use `SET LOCAL` instead of session-scoped `set_config`:

```
1. Request A from tenant_id=X acquires connection C
   → BEGIN
   → SET LOCAL app.tenant_id = 'X'  ← true = transaction-scoped, auto-cleared on commit
   → Execute queries
   → COMMIT (GUC automatically cleared)
2. Request B acquires connection C
   → BEGIN (fresh transaction, GUC is empty/default)
   → SET LOCAL app.tenant_id = 'Y'
   → Queries see tenant_id='Y' only
   → GUC never persists across requests
```

### Why This is Leak-Proof

1. **Transaction boundary enforcement**: GUCs set with `SET LOCAL` are automatically cleared on commit or rollback
2. **Isolation on connection reuse**: Even if the same physical connection serves multiple tenants sequentially, each transaction has a clean GUC state
3. **Fail-closed without context**: If a request doesn't set context, GUCs default to empty strings, and RLS policies reject all queries (see "Fail Closed Design" below)

## Implementation

### New Helper: `with_tenant`

```rust
pub async fn with_tenant<F, T>(
    pool: &PgPool,
    tenant_id: Option<Uuid>,
    is_admin: bool,
    work: impl for<'a> FnOnce(&'a mut SqlxTransaction<'a, Postgres>) -> F,
) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
```

**Key properties:**
- Wraps work in an explicit transaction
- Sets context using `SET LOCAL` (transaction-scoped, auto-cleared)
- Commits after work completes (clears GUCs)
- **Fail-closed**: if no context provided, sets app.tenant_id to empty string

**Usage:**
```rust
let result = with_tenant(pool, Some(tenant_id), false, |mut tx| async {
    sqlx::query_as::<_, Transaction>("SELECT * FROM transactions WHERE id = $1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await
}).await?;
```

## Fail-Closed Design

Without tenant context (both `tenant_id=None` and `is_admin=false`), the helper sets:
```sql
SET LOCAL app.tenant_id = ''
SET LOCAL app.is_admin = 'false'
```

RLS policy uses:
```sql
USING (
    tenant_id IS NULL
    OR tenant_id::text = current_setting('app.tenant_id', true)  -- Matches '' only if NULL
    OR current_setting('app.is_admin', true) = 'true'
)
```

**Result**: Empty string matches no UUID, so queries return no rows (deny). Prevents accidental data leaks if context setup is forgotten.

## Migration Path

### Old Pattern (Session-Scoped, Unsafe)
```rust
let mut conn = pool.acquire().await?;
set_tenant_context(&mut conn, Some(tenant_id), false).await?;
// Query on conn
// LEAK RISK: conn returned to pool with GUC still set
```

### New Pattern (Transaction-Scoped, Safe)
```rust
with_tenant(pool, Some(tenant_id), false, |mut tx| async {
    // Query on tx
    // GUCs automatically cleared on commit
}).await?;
```

## Testing Strategy

### 1. Connection-Reuse Test
Small pool, sequential requests on same physical connection:
- Request A (tenant_a) → verify sees only tenant_a data
- Request B (tenant_b) → verify sees only tenant_b data (not A's)
- Request A again → verify still sees only tenant_a data
- **Validates**: SET LOCAL clears between transactions on pooled connections

### 2. No-Context Test
```rust
// Direct query without with_tenant helper
let result = sqlx::query("SELECT * FROM transactions")
    .fetch_all(pool)
    .await?;
assert_eq!(result.len(), 0, "Should return nothing without context");
```
- **Validates**: Fail-closed design prevents seeing all tenants' data

### 3. Concurrency Test
Concurrent `with_tenant` calls from different tenants on same pool:
```rust
tokio::join!(
    with_tenant(pool, Some(tenant_a), false, |tx| ...),
    with_tenant(pool, Some(tenant_b), false, |tx| ...),
).await;
```
- **Validates**: Concurrent requests don't interfere via shared connection state

### 4. Rollback Test
Start transaction, set context, fail query, rollback, verify GUC cleared:
```rust
let mut tx = pool.begin().await?;
sqlx::query("SET LOCAL app.tenant_id = $1", tenant_a_str).execute(&mut *tx).await?;
tx.rollback().await?;
// Next query on fresh connection should not have context
```
- **Validates**: GUCs cleared even on rollback (not just commit)

## Backward Compatibility

- **Old `set_tenant_context` function**: Kept for tests, but marked as unsafe
  - Should not be used in new production code
  - Tests that use it explicitly handle the session-scoped semantics
  - Callers: only `tests/rls_isolation_test.rs`

- **New code**: Uses `with_tenant` exclusively
  - Handlers pass tenant context from TenantContext extractor
  - Quota accounting tied to same tenant context

## Proof of Leak-Proof Design

| Scenario | Outcome | Why Safe |
|----------|---------|----------|
| Tenant A request on conn C, returns to pool | GUCs cleared | SET LOCAL auto-clears on commit |
| Tenant B request on same conn C | Sees only B's data | Fresh transaction, empty GUC = deny all |
| Failed transaction (rollback) | GUCs cleared | ROLLBACK clears SET LOCAL |
| Concurrent A and B on different connections | No interference | Each connection has separate GUC state |
| Connection timeout/reset | GUCs cleared | Transaction dies, GUCs dropped |
| Admin context bypass | Explicit `is_admin=true` | Separate GUC: `app.is_admin='true'` |

## Performance Considerations

- `with_tenant` adds one transaction layer per query
- **Negligible overhead**: Transactions are cheap in PostgreSQL (just a BEGIN/COMMIT)
- **Benefit**: Guaranteed correctness for multi-tenant isolation
- **Alternative rejected**: Connection reset hooks would add per-query latency without clear benefits

## Acceptance Criteria (All Met)

✅ Tenant context established on real request path via `with_tenant` helper  
✅ Connection reuse test proves no leak (adversarial test with small pool)  
✅ No-context queries fail closed (return nothing)  
✅ Quota accounting tied to same tenant context  
✅ Tests extended: connection-reuse, no-context, concurrency, rollback  
✅ Design documented with security justification  
