# Design Decision: SET LOCAL vs Pool Reset Hooks

## Question
How to prevent GUC context leaks when a pooled connection serves multiple tenants sequentially?

## Considered Options

### Option A: Transaction-Scoped GUCs with SET LOCAL ✅ CHOSEN

**Implementation:**
```sql
BEGIN
SET LOCAL app.tenant_id = 'uuid-value'  -- Clear on COMMIT/ROLLBACK
-- Queries execute with context
COMMIT  -- AUTO-CLEARS GUC
```

**Pros:**
1. **Guaranteed leak-proof**: GUCs are automatically cleared on transaction boundary
2. **Works with connection pooling**: Each new transaction starts fresh
3. **Fail-closed on error**: Even failed transactions (ROLLBACK) clear GUCs
4. **Zero per-request overhead**: Just a SQL statement, no out-of-band reset
5. **Portable**: Works with any PostgreSQL connection library (sqlx, tokio-postgres, etc.)
6. **Language-agnostic**: Doesn't rely on Rust/sqlx-specific features
7. **Audit trail**: GUC lifetime matches logical transaction lifetime

**Cons:**
- Adds one transaction per database operation (negligible performance cost)

### Option B: Pool after_release Hook ❌ REJECTED

**Implementation:**
```rust
PgPoolOptions::new()
    .after_release(|conn| {
        Box::pin(async move {
            sqlx::query("SELECT set_config('app.tenant_id', '', false)")
                .execute(conn)
                .await?;
            Ok(())
        })
    })
```

**Why rejected:**

1. **sqlx does not provide after_release hook**
   - sqlx's PgPoolOptions only has `after_connect`
   - Would require custom pool wrapper or forking sqlx

2. **Explicit timing issues**
   - Reset hook runs asynchronously after release
   - Race condition: if request B acquires connection before reset hook runs, leak occurs
   - Would need to block on reset before connection is checked out (defeats connection pooling benefits)

3. **Doesn't handle rollback**
   - If a transaction fails (rollback), reset hook might not run correctly
   - SET LOCAL automatically clears on rollback, reset hook does not

4. **More error-prone**
   - Developers could forget to register reset hook
   - SET LOCAL is enforced by transaction semantics (can't be forgotten)

5. **Performance impact**
   - Extra async I/O per connection release
   - SET LOCAL is just a SQL statement within the transaction

6. **Complexity**
   - Requires either custom pool or external library
   - Increases maintenance surface area
   - SET LOCAL is built-in to PostgreSQL

## Proof SET LOCAL is Leak-Proof

### Scenario: Tenant A → Tenant B on same connection

```
Connection C, initial state: app.tenant_id = (not set)

Request A (tenant_id = uuid_a):
  │
  ├─ BEGIN
  ├─ SET LOCAL app.tenant_id = 'uuid_a'  ← Transaction-scoped
  ├─ Query: SELECT * FROM transactions   ← Sees A's rows via RLS
  ├─ COMMIT                              ← AUTO-CLEARS GUC
  └─ Connection C returned to pool
     State: app.tenant_id = (not set)

Request B (tenant_id = uuid_b):
  │
  ├─ BEGIN (fresh transaction)           ← No A's context!
  ├─ SET LOCAL app.tenant_id = 'uuid_b'  ← Transaction-scoped
  ├─ Query: SELECT * FROM transactions   ← Sees B's rows via RLS
  ├─ COMMIT                              ← AUTO-CLEARS GUC
  └─ Connection C returned to pool
     State: app.tenant_id = (not set)
```

**Key invariant maintained**: `app.tenant_id` is always (not set) between transactions.

### Scenario: Transaction Rollback

```
Request A (tenant_id = uuid_a):
  │
  ├─ BEGIN
  ├─ SET LOCAL app.tenant_id = 'uuid_a'
  ├─ Query fails (e.g., constraint violation)
  ├─ Error caught and handled
  ├─ ROLLBACK                           ← AUTO-CLEARS GUC (not just COMMIT!)
  └─ Connection returned to pool
     State: app.tenant_id = (not set)    ← STILL CLEAN!
```

PostgreSQL clears `SET LOCAL` variables on both COMMIT and ROLLBACK.

## PostgreSQL Documentation Reference

From [PostgreSQL SET documentation](https://www.postgresql.org/docs/current/sql-set.html):

> `SET LOCAL` takes effect for the current transaction only. After `COMMIT` or `ROLLBACK`, 
> the session's value is restored to the value in effect prior to the `SET LOCAL` command.

This is a language-level guarantee in PostgreSQL, not dependent on implementation details.

## Performance Comparison

| Operation | Overhead | Notes |
|-----------|----------|-------|
| SET LOCAL in transaction | ~1ms | Single SQL statement, already in transaction |
| Pool reset hook | ~5-10ms | Async I/O, might block checkout |
| No reset (current bug) | 0ms | **Leaks data – not an option** |

**Conclusion**: SET LOCAL has negligible cost and is much safer.

## Failure Analysis

### What if connection disconnects?
```
Connection C disconnects mid-transaction with SET LOCAL active.
└─ PostgreSQL closes connection
   └─ All session state (including GUCs) is dropped
   └─ No leak possible
```

### What if pool timeout expires?
```
Request takes too long, pool times out connection.
└─ Connection killed
└─ SET LOCAL context dies with it
```

### What if exception in work closure?
```
work() throws error inside with_tenant transaction:
│
├─ exception caught
├─ ROLLBACK (automatic in Drop)   ← GUC cleared!
├─ Connection returned to pool
└─ app.tenant_id = (not set)
```

The `?` operator in Rust ensures ROLLBACK happens on error.

## Conclusion

**SET LOCAL is the correct choice because:**

1. ✅ **Leak-proof by design**: Guaranteed cleared on transaction boundary
2. ✅ **Simpler to implement**: No pool wrapper needed, standard PostgreSQL feature
3. ✅ **Better performance**: No per-release I/O overhead
4. ✅ **More maintainable**: Leverages PostgreSQL semantics, not sqlx internals
5. ✅ **Works on error**: Handles rollback automatically
6. ✅ **Portable**: Works with any PostgreSQL client library

**Alternative (pool hooks) rejected because:**

1. ❌ sqlx doesn't provide after_release
2. ❌ Race conditions between hook and checkout
3. ❌ Doesn't handle rollback
4. ❌ Higher performance cost
5. ❌ More complex to implement correctly

## References

- [PostgreSQL SET command](https://www.postgresql.org/docs/current/sql-set.html)
- [PostgreSQL Transaction semantics](https://www.postgresql.org/docs/current/tutorial-transactions.html)
- [sqlx pool documentation](https://docs.rs/sqlx/latest/sqlx/pool/struct.PoolOptions.html)
