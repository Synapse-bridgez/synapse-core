# ADR-003: Multi-Tenant Isolation Strategy

## Status

Accepted

## Context

Synapse Core needs to support multiple Stellar Anchor Platform integrations on a single deployment. Each anchor (tenant) requires:

- **Data isolation** - Tenants cannot access each other's transaction data
- **Configuration isolation** - Each tenant has unique webhook secrets, API keys, Stellar accounts
- **Rate limiting** - Per-tenant rate limits to prevent abuse
- **Authentication** - Secure API key-based authentication per tenant
- **Scalability** - Support for hundreds of tenants without performance degradation

Without proper multi-tenancy, we would need:
- Separate deployments per anchor (high operational overhead)
- Separate databases per anchor (expensive, complex)
- Manual configuration management
- No shared infrastructure benefits

**Security requirements:**
- Prevent cross-tenant data access (accidental or malicious)
- Ensure tenant isolation at multiple layers (application, database)
- Audit trail for all tenant data access
- Secure credential storage

## Decision

We will implement **shared database, shared schema multi-tenancy** with **application-level tenant isolation** enforced by:

1. **Tenant context extraction** - Automatic tenant identification from API keys
2. **Query-level filtering** - All database queries require and filter by `tenant_id`
3. **Row-Level Security (RLS)** - PostgreSQL RLS as defense-in-depth
4. **API key authentication** - Each tenant has a unique API key
5. **Configuration caching** - In-memory tenant config cache for performance

**Architecture:**

```
Request → API Key → Tenant Context → Query (with tenant_id) → Database (RLS)
```

**Tenant resolution order:**
1. URL path parameter (if present)
2. `X-API-Key` or `Authorization: Bearer` header
3. `X-Tenant-ID` header (for internal services)

**Database schema:**

```sql
-- Tenants table
CREATE TABLE tenants (
    tenant_id UUID PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    api_key VARCHAR(255) NOT NULL UNIQUE,
    webhook_secret VARCHAR(255) NOT NULL,
    stellar_account VARCHAR(56) NOT NULL,
    rate_limit_per_minute INTEGER NOT NULL DEFAULT 60,
    is_active BOOLEAN NOT NULL DEFAULT true
);

-- Transactions table with tenant_id foreign key
CREATE TABLE transactions (
    transaction_id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL REFERENCES tenants(tenant_id) ON DELETE CASCADE,
    -- ... other fields
    CONSTRAINT unique_external_id_per_tenant UNIQUE (tenant_id, external_id)
);

-- Row Level Security
ALTER TABLE transactions ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON transactions
    USING (tenant_id = current_setting('app.current_tenant_id')::UUID);
```

## Consequences

### Positive

- **Cost efficiency** - Single deployment serves all tenants
- **Operational simplicity** - One database, one application to maintain
- **Resource sharing** - Connection pooling, caching shared across tenants
- **Easy onboarding** - Add new tenant with single database INSERT
- **Consistent updates** - All tenants get updates simultaneously
- **Centralized monitoring** - Single metrics/logging system
- **Defense in depth** - Multiple layers of isolation (app + database)
- **Scalability** - Can support hundreds of tenants

### Negative

- **Noisy neighbor risk** - One tenant's load can affect others
- **Security complexity** - Must ensure tenant_id filtering in all queries
- **Testing overhead** - Must test cross-tenant isolation
- **Migration complexity** - Schema changes affect all tenants
- **Backup granularity** - Cannot backup individual tenants easily
- **Compliance challenges** - Some regulations may require physical isolation

### Neutral

- **Performance** - Comparable to single-tenant with proper indexing
- **Query complexity** - All queries need tenant_id parameter
- **Code patterns** - Consistent tenant filtering required

## Alternatives Considered

### Alternative 1: Separate Database Per Tenant

**Description:** Each tenant gets their own PostgreSQL database.

**Pros:**
- Perfect isolation (physical separation)
- Easy to backup/restore per tenant
- Can scale tenants independently
- Simpler queries (no tenant_id filtering)
- Compliance-friendly

**Cons:**
- High operational overhead (manage N databases)
- Expensive (N connection pools, N backups)
- Difficult to query across tenants (analytics)
- Schema migrations must run N times
- Resource waste (small tenants over-provisioned)

**Why rejected:** Operational complexity and cost don't justify the benefits at our scale. We can achieve sufficient isolation with shared database.

### Alternative 2: Separate Schema Per Tenant

**Description:** Each tenant gets their own PostgreSQL schema within one database.

**Pros:**
- Good isolation (schema-level)
- Easier than separate databases
- Can backup per schema
- Simpler queries (no tenant_id filtering)

**Cons:**
- Still complex to manage (N schemas)
- Schema migrations must run N times
- Connection pooling complexity (set search_path per connection)
- PostgreSQL limits on schema count
- Difficult to query across tenants

**Why rejected:** Middle ground that inherits complexity from both approaches. Shared schema with RLS provides similar isolation with less complexity.

### Alternative 3: Discriminator Column Only (No RLS)

**Description:** Use `tenant_id` column with application-level filtering only, no Row-Level Security.

**Pros:**
- Simpler implementation
- Better query performance (no RLS overhead)
- Easier to debug

**Cons:**
- Single point of failure (forget tenant_id filter = data leak)
- No defense in depth
- Difficult to audit
- Higher security risk

**Why rejected:** Security risk too high. RLS provides critical defense-in-depth at minimal performance cost.

### Alternative 4: Separate Deployment Per Tenant

**Description:** Each tenant gets their own application deployment and database.

**Pros:**
- Perfect isolation (physical + logical)
- Independent scaling
- No noisy neighbor issues
- Tenant-specific customization easy

**Cons:**
- Extremely high operational overhead
- Very expensive (N deployments, N databases)
- Difficult to maintain consistency
- Slow onboarding (provision infrastructure per tenant)
- Waste of resources

**Why rejected:** Completely impractical at scale. Only viable for very small number of large enterprise customers.

### Alternative 5: Hybrid Approach (Tenant Pools)

**Description:** Group tenants into pools, each pool has separate database.

**Pros:**
- Balance between isolation and efficiency
- Can isolate high-value tenants
- Limits blast radius

**Cons:**
- Complex tenant assignment logic
- Difficult to rebalance
- Multiple databases to maintain
- Unclear when to create new pool

**Why rejected:** Premature optimization. We can start with shared database and move specific tenants to dedicated databases if needed.

## Implementation Notes

### Tenant Context Extractor

```rust
#[async_trait]
impl<S> FromRequestParts<S> for TenantContext
where
    S: Send + Sync,
    AppState: FromRef<S>,
{
    type Rejection = (StatusCode, Json<ErrorResponse>);

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app_state = AppState::from_ref(state);
        
        // Extract API key from headers
        let api_key = extract_api_key(parts)?;
        
        // Lookup tenant by API key
        let tenant = sqlx::query_as!(
            Tenant,
            "SELECT * FROM tenants WHERE api_key = $1 AND is_active = true",
            api_key
        )
        .fetch_optional(&app_state.pool)
        .await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, Json(ErrorResponse::internal())))?
        .ok_or((StatusCode::UNAUTHORIZED, Json(ErrorResponse::unauthorized())))?;
        
        Ok(TenantContext {
            tenant_id: tenant.tenant_id,
            config: tenant.into(),
        })
    }
}
```

### Query Pattern

**All queries must follow this pattern:**

```rust
pub async fn get_transaction(
    pool: &PgPool,
    tenant_id: Uuid,  // ALWAYS required
    transaction_id: Uuid,
) -> Result<Transaction, AppError> {
    sqlx::query_as!(
        Transaction,
        r#"
        SELECT * FROM transactions
        WHERE transaction_id = $1 AND tenant_id = $2  -- ALWAYS filter by tenant_id
        "#,
        transaction_id,
        tenant_id
    )
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::TransactionNotFound)
}
```

### Row-Level Security Setup

```sql
-- Enable RLS on transactions table
ALTER TABLE transactions ENABLE ROW LEVEL SECURITY;

-- Create policy for tenant isolation
CREATE POLICY tenant_isolation ON transactions
    USING (tenant_id = current_setting('app.current_tenant_id')::UUID);

-- Set tenant context before queries (in application)
-- SET LOCAL app.current_tenant_id = 'tenant-uuid-here';
```

**Note:** RLS is defense-in-depth. Application-level filtering is primary mechanism.

### Configuration Caching

```rust
pub struct AppState {
    pub pool: PgPool,
    pub tenant_configs: Arc<RwLock<HashMap<Uuid, TenantConfig>>>,
}

impl AppState {
    pub async fn load_tenant_configs(&self) -> Result<()> {
        let tenants = sqlx::query_as!(
            Tenant,
            "SELECT * FROM tenants WHERE is_active = true"
        )
        .fetch_all(&self.pool)
        .await?;
        
        let mut configs = self.tenant_configs.write().await;
        for tenant in tenants {
            configs.insert(tenant.tenant_id, tenant.into());
        }
        
        Ok(())
    }
}
```

### Testing Tenant Isolation

```rust
#[tokio::test]
async fn test_tenant_cannot_access_other_tenant_data() {
    let pool = setup_test_pool().await;
    
    // Create two tenants
    let tenant1_id = create_test_tenant(&pool, "tenant1").await;
    let tenant2_id = create_test_tenant(&pool, "tenant2").await;
    
    // Create transaction for tenant1
    let tx_id = create_test_transaction(&pool, tenant1_id).await;
    
    // Try to access with tenant2 credentials
    let result = get_transaction(&pool, tenant2_id, tx_id).await;
    
    // Should return NotFound (not Unauthorized to avoid leaking existence)
    assert!(matches!(result, Err(AppError::TransactionNotFound)));
}
```

### Security Checklist

- [ ] All queries include `tenant_id` filter
- [ ] TenantContext extractor validates tenant is active
- [ ] API keys are unique and indexed
- [ ] Row-Level Security enabled on all tenant-scoped tables
- [ ] Unique constraints scoped per tenant
- [ ] Foreign keys use CASCADE delete
- [ ] Integration tests verify cross-tenant isolation
- [ ] Audit logging tracks tenant data access

### Performance Considerations

**Indexes:**
```sql
-- Critical for tenant filtering performance
CREATE INDEX idx_transactions_tenant_id ON transactions(tenant_id);
CREATE INDEX idx_transactions_tenant_status ON transactions(tenant_id, status);
CREATE INDEX idx_transactions_tenant_created ON transactions(tenant_id, created_at DESC);
```

**Query patterns:**
- Always filter by `tenant_id` first (most selective)
- Use composite indexes for common query patterns
- Monitor slow queries per tenant

**Connection pooling:**
- Shared pool across all tenants
- Monitor pool utilization per tenant
- Consider separate pool for high-volume tenants if needed

### Monitoring

**Metrics to track:**
- Requests per tenant
- Transaction volume per tenant
- API key usage patterns
- Failed authentication attempts per tenant
- Query latency per tenant
- Storage usage per tenant

**Alerts:**
- Unusual cross-tenant access patterns
- High failure rate for specific tenant
- Tenant approaching rate limit
- Inactive tenant with API key usage

## References

- [Multi-Tenancy Patterns (Microsoft)](https://docs.microsoft.com/en-us/azure/architecture/patterns/multi-tenancy)
- [PostgreSQL Row Level Security](https://www.postgresql.org/docs/current/ddl-rowsecurity.html)
- [src/Multi-Tenant Isolation Layer (Architecture)/IMPLEMENTATION_GUIDE.md](../../src/Multi-Tenant%20Isolation%20Layer%20(Architecture)/IMPLEMENTATION_GUIDE.md)
- [Designing Data-Intensive Applications (Martin Kleppmann)](https://dataintensive.net/)
- Issue #XX - Multi-tenant implementation
