# Integrating Tenant Context into Handlers

This document shows how to use the new `with_tenant` helper in request handlers to establish tenant context for all database operations.

## Pattern: Extract Tenant, Then Query

### Before (Unsafe Session-Scoped)
```rust
async fn list_transactions(
    State(state): State<AppState>,
    TenantContext { tenant_id, .. }: TenantContext,
) -> Result<Response, AppError> {
    // UNSAFE: set_tenant_context uses session-scoped GUCs (can leak across requests)
    let mut conn = state.db.acquire().await?;
    set_tenant_context(&mut conn, Some(tenant_id), false).await?;
    
    let txns = sqlx::query_as::<_, Transaction>(
        "SELECT * FROM transactions WHERE tenant_id = $1"
    )
    .bind(tenant_id)
    .fetch_all(&mut *conn)
    .await?;
    
    Ok(Json(txns).into_response())
}
```

### After (Safe Transaction-Scoped)
```rust
use crate::db::queries::with_tenant;

async fn list_transactions(
    State(state): State<AppState>,
    TenantContext { tenant_id, .. }: TenantContext,
) -> Result<Response, AppError> {
    // SAFE: with_tenant wraps work in a transaction with SET LOCAL context
    let txns = with_tenant(&state.db, Some(tenant_id), false, |mut tx| async {
        sqlx::query_as::<_, Transaction>(
            "SELECT * FROM transactions"  ← RLS policy handles filtering
        )
        .fetch_all(&mut **tx)
        .await
    })
    .await?;
    
    Ok(Json(txns).into_response())
}
```

## Key Differences

| Aspect | Old (Session-Scoped) | New (Transaction-Scoped) |
|--------|----------------------|--------------------------|
| **Context Lifetime** | Persists on connection | Auto-cleared on commit |
| **Connection Reuse** | Leak risk on reuse | Safe, fresh context each time |
| **Syntax** | Manual set + query | Helper manages transaction |
| **Fail-Closed** | Manual, easy to forget | Built-in: empty context denies all |

## Pattern 1: Read Query (Tenant-Scoped)

```rust
use crate::db::queries::with_tenant;

#[instrument(skip(state))]
pub async fn get_transaction(
    State(state): State<AppState>,
    TenantContext { tenant_id, .. }: TenantContext,
    Path(tx_id): Path<Uuid>,
) -> Result<Json<Transaction>, AppError> {
    let tx = with_tenant(&state.db, Some(tenant_id), false, |mut tx| async {
        sqlx::query_as::<_, Transaction>(
            "SELECT * FROM transactions WHERE id = $1"
        )
        .bind(tx_id)
        .fetch_optional(&mut **tx)
        .await
    })
    .await?
    .ok_or(AppError::NotFound)?;
    
    Ok(Json(tx))
}
```

**What happens:**
1. Request comes in with TenantContext extracted
2. `with_tenant` creates a transaction
3. Sets `app.tenant_id = tenant_id` with `SET LOCAL` (transaction-scoped)
4. Executes query → RLS policy sees context and filters by tenant
5. Commits (GUC auto-cleared)
6. Connection returned to pool clean

## Pattern 2: Write Query (Insert with Tenant)

```rust
pub async fn create_transaction(
    State(state): State<AppState>,
    TenantContext { tenant_id, .. }: TenantContext,
    Json(payload): Json<TransactionPayload>,
) -> Result<(StatusCode, Json<Transaction>), AppError> {
    let new_tx = with_tenant(&state.db, Some(tenant_id), false, |mut tx| async {
        // Validate input
        payload.validate()?;
        
        let tx_model = Transaction::new(
            Uuid::new_v4(),
            tenant_id,
            payload.stellar_account,
            payload.amount,
            payload.asset_code,
        );
        
        // Insert with tenant context set
        sqlx::query_as::<_, Transaction>(
            r#"INSERT INTO transactions (...) 
               VALUES (...) 
               RETURNING *"#
        )
        .bind(tx_model.id)
        .bind(tenant_id)
        // ... other binds
        .fetch_one(&mut **tx)
        .await
    })
    .await?;
    
    Ok((StatusCode::CREATED, Json(new_tx)))
}
```

## Pattern 3: Admin Query (Bypass RLS)

```rust
pub async fn admin_get_all_transactions(
    State(state): State<AppState>,
    _admin: AdminAuth,  ← Ensures authorized admin
) -> Result<Json<Vec<Transaction>>, AppError> {
    let txns = with_tenant(&state.db, None, true, |mut tx| async {
        sqlx::query_as::<_, Transaction>(
            "SELECT * FROM transactions"  ← No WHERE clause
        )
        .fetch_all(&mut **tx)
        .await
    })
    .await?;
    
    Ok(Json(txns))
}
```

**Key difference:**
- `with_tenant(..., None, true, ...)` → sets `app.is_admin='true'`
- RLS policy sees `is_admin=true` and allows bypassing tenant filter

## Pattern 4: Quota Accounting (Tenant Attribution)

Quota middleware should also extract tenant context:

```rust
async fn quota_check_middleware(
    State(state): State<AppState>,
    TenantContext { tenant_id, config }: TenantContext,
    req: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    // Check quota for this specific tenant
    let key = format!("quota:tenant:{}", tenant_id);
    
    if !state.quota_manager.consume_quota(&key).await? {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    
    // Track quota consumption tied to tenant_id
    // (not to a generic "user" key)
    
    Ok(next.run(req).await)
}
```

## Migration Checklist

For each handler that queries databases:

- [ ] Extract `TenantContext` from request
- [ ] Replace `acquire()` + `set_tenant_context()` with `with_tenant(..., |mut tx| async { ... })`
- [ ] Remove manual context-setting logic
- [ ] RLS policies now handle filtering (no manual WHERE clause needed for tenant)
- [ ] Verify tests cover both tenant A and tenant B data isolation

## Common Mistakes

### ❌ Mistake 1: Mixing patterns
```rust
// DON'T do this:
let mut conn = pool.acquire().await?;
set_tenant_context(&mut conn, Some(tenant_id), false).await?;

let result = with_tenant(&pool, ..., |tx| async { ... }).await?;
// Context set twice, redundant
```

### ❌ Mistake 2: Forgetting context entirely
```rust
// DON'T do this:
let result = sqlx::query_as::<_, Transaction>(
    "SELECT * FROM transactions"
)
.fetch_all(&pool)  ← No with_tenant! Queries without context fail closed (return nothing)
.await?;
```

### ✅ Correct: Single with_tenant wrapper
```rust
// DO this:
let result = with_tenant(&pool, Some(tenant_id), false, |mut tx| async {
    sqlx::query_as::<_, Transaction>("SELECT * FROM transactions")
        .fetch_all(&mut **tx)
        .await
})
.await?;
```

## Testing Helpers

```rust
#[cfg(test)]
mod tests {
    use crate::db::queries::with_tenant;
    
    #[tokio::test]
    async fn test_handler_respects_tenant_isolation() {
        let state = make_test_state().await;
        let tenant_a = Uuid::new_v4();
        let tenant_b = Uuid::new_v4();
        
        // Insert data for both tenants
        insert_test_transaction(state.db, tenant_a, "tx_a").await;
        insert_test_transaction(state.db, tenant_b, "tx_b").await;
        
        // Tenant A should not see B's data
        let ctx_a = TenantContext { tenant_id: tenant_a, ..  };
        let response_a = list_transactions(
            State(state.clone()),
            ctx_a,
        ).await.unwrap();
        
        assert_eq!(response_a.len(), 1);
        assert_eq!(response_a[0].id, "tx_a");
    }
}
```
