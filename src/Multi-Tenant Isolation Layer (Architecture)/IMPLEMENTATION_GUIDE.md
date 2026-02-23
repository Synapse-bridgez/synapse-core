# Multi-Tenant Implementation Guide

This document explains the multi-tenant architecture implementation for Synapse Core.

## Overview

The implementation provides complete tenant isolation for multiple Anchor Platform integrations on a single deployment. Each tenant has:

- Isolated transaction data
- Individual configuration (webhook secrets, Stellar accounts, rate limits)
- Separate API keys for authentication
- Row-level security at the database layer

## Key Components

### 1. Tenant Resolution (`src/tenant/mod.rs`)

The `TenantContext` extractor automatically identifies and validates tenants from incoming requests.

**Resolution Order:**

1. URL path parameter (if present)
2. `X-API-Key` or `Authorization: Bearer` header
3. `X-Tenant-ID` header

**Security Checks:**

- Validates tenant exists in database
- Checks `is_active` flag
- Loads tenant configuration
- Returns 401 Unauthorized if validation fails

```rust
// Automatic extraction in handlers
pub async fn create_transaction(
    tenant: TenantContext,  // Automatically resolved
    Json(req): Json<CreateTransactionRequest>,
) -> Result<Json<TransactionResponse>> {
    // tenant.tenant_id is guaranteed to be valid
    // tenant.config contains webhook_secret, stellar_account, etc.
}
```

### 2. Database Queries (`src/db/queries.rs`)

All query functions enforce tenant isolation by requiring `tenant_id` as a parameter.

**Critical Pattern:**

```rust
pub async fn get_transaction(
    pool: &PgPool,
    tenant_id: Uuid,  // Always required
    transaction_id: Uuid,
) -> Result<Transaction> {
    sqlx::query_as!(
        Transaction,
        r#"
        SELECT * FROM transactions
        WHERE transaction_id = $1 AND tenant_id = $2  -- Always filter by tenant_id
        "#,
        transaction_id,
        tenant_id
    )
    .fetch_optional(pool)
    .await?
    .ok_or(AppError::TransactionNotFound)
}
```

**Why This Matters:**

- Prevents accidental cross-tenant data access
- Makes security review straightforward
- Compiler enforces tenant_id parameter
- Database indexes optimize tenant-filtered queries

### 3. Database Schema

#### Tenants Table

Stores per-tenant configuration:

```sql
CREATE TABLE tenants (
    tenant_id UUID PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    api_key VARCHAR(255) NOT NULL UNIQUE,
    webhook_secret VARCHAR(255) NOT NULL,
    stellar_account VARCHAR(56) NOT NULL,
    rate_limit_per_minute INTEGER NOT NULL DEFAULT 60,
    is_active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
```

**Indexes:**

- `idx_tenants_api_key` - Fast API key lookups
- `idx_tenants_active` - Filter active tenants

#### Transactions Table

Stores transaction data with tenant isolation:

```sql
CREATE TABLE transactions (
    transaction_id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL REFERENCES tenants(tenant_id) ON DELETE CASCADE,
    external_id VARCHAR(255) NOT NULL,
    -- ... other fields
    CONSTRAINT unique_external_id_per_tenant UNIQUE (tenant_id, external_id)
);
```

**Key Features:**

- Foreign key to `tenants` with CASCADE delete
- Unique constraint scoped per tenant
- Row Level Security enabled

**Indexes:**

- `idx_transactions_tenant_id` - Primary isolation filter
- `idx_transactions_tenant_status` - Status queries per tenant
- `idx_transactions_tenant_created` - Chronological listing per tenant

### 4. API Handlers (`src/api/handlers.rs`)

Handlers receive validated `TenantContext` and pass `tenant_id` to all queries:

```rust
pub async fn list_transactions(
    State(state): State<AppState>,
    tenant: TenantContext,  // Validated by extractor
    Query(params): Query<PaginationParams>,
) -> Result<Json<TransactionListResponse>> {
    // Pass tenant_id to query - enforces isolation
    let transactions = queries::list_transactions(
        &state.pool,
        tenant.tenant_id,  // Always use this
        params.limit,
        params.offset,
    )
    .await?;

    Ok(Json(TransactionListResponse {
        transactions,
        total: transactions.len(),
    }))
}
```

### 5. Configuration Management (`src/config.rs`)

`AppState` caches tenant configurations in memory:

```rust
pub struct AppState {
    pub pool: PgPool,
    pub tenant_configs: Arc<RwLock<HashMap<Uuid, TenantConfig>>>,
}
```

**Benefits:**

- Fast config lookups without database queries
- Reload configs without restart
- Thread-safe with RwLock

**Usage:**

```rust
// Load all tenant configs at startup
app_state.load_tenant_configs().await?;

// Get config for specific tenant
if let Some(config) = app_state.get_tenant_config(tenant_id).await {
    // Use config.webhook_secret, config.stellar_account, etc.
}
```

## Security Model

### Defense in Depth

1. **Application Layer**: TenantContext extractor validates tenant
2. **Query Layer**: All queries require and filter by tenant_id
3. **Database Layer**: Row Level Security as additional safeguard
4. **Schema Layer**: Foreign key constraints prevent orphaned data

### Preventing Cross-Tenant Access

**Bad (Vulnerable):**

```rust
// DON'T DO THIS - No tenant filtering
sqlx::query!("SELECT * FROM transactions WHERE transaction_id = $1", id)
```

**Good (Secure):**

```rust
// ALWAYS filter by tenant_id
sqlx::query!(
    "SELECT * FROM transactions WHERE transaction_id = $1 AND tenant_id = $2",
    id,
    tenant_id
)
```

### API Key Security

- Store API keys securely (consider hashing in production)
- Rotate keys periodically
- Use HTTPS in production
- Consider adding key expiration

### Webhook Security

Each tenant has a unique `webhook_secret` for HMAC validation:

```rust
// TODO: Implement in webhook handler
fn validate_webhook_signature(
    payload: &[u8],
    signature: &str,
    secret: &str,
) -> bool {
    // HMAC-SHA256 validation
}
```

## Testing Tenant Isolation

### Manual Testing

1. **Create transactions for different tenants:**

```bash
# Tenant 1
curl -X POST http://localhost:3000/api/transactions \
  -H "X-API-Key: demo_api_key_anchor_platform_001" \
  -H "Content-Type: application/json" \
  -d '{"external_id": "t1_001", "amount": "100", "asset_code": "USDC"}'

# Tenant 2
curl -X POST http://localhost:3000/api/transactions \
  -H "X-API-Key: test_api_key_partner_002" \
  -H "Content-Type: application/json" \
  -d '{"external_id": "t2_001", "amount": "200", "asset_code": "USDC"}'
```

2. **Verify isolation:**

```bash
# Tenant 1 should only see their transaction
curl http://localhost:3000/api/transactions \
  -H "X-API-Key: demo_api_key_anchor_platform_001"

# Tenant 2 should only see their transaction
curl http://localhost:3000/api/transactions \
  -H "X-API-Key: test_api_key_partner_002"
```

3. **Test cross-tenant access prevention:**

```bash
# Try to access Tenant 1's transaction with Tenant 2's key
# Should return 404 Not Found
curl http://localhost:3000/api/transactions/{tenant1_transaction_id} \
  -H "X-API-Key: test_api_key_partner_002"
```

### Automated Testing

Add integration tests in `tests/tenant_isolation.rs`:

```rust
#[tokio::test]
async fn test_tenant_cannot_access_other_tenant_data() {
    // Create transaction for tenant 1
    // Try to access with tenant 2 credentials
    // Assert 404 or 403 error
}

#[tokio::test]
async fn test_inactive_tenant_rejected() {
    // Try to use inactive tenant API key
    // Assert 401 Unauthorized
}
```

## Production Deployment

### Environment Variables

```bash
DATABASE_URL=postgresql://user:password@host:5432/synapse_core
RUST_LOG=info,synapse_core=debug
```

### Database Setup

1. Create database:

```bash
createdb synapse_core
```

2. Run migrations:

```bash
chmod +x run_migrations.sh
./run_migrations.sh
```

3. Verify schema:

```bash
psql $DATABASE_URL -c "\dt"
psql $DATABASE_URL -c "\d tenants"
psql $DATABASE_URL -c "\d transactions"
```

### Monitoring

Add metrics for:

- Requests per tenant
- Transaction volume per tenant
- API key usage
- Failed authentication attempts
- Query performance per tenant

### Scaling Considerations

1. **Database Connection Pooling**: Adjust based on tenant count
2. **Caching**: Cache tenant configs (already implemented)
3. **Rate Limiting**: Implement per-tenant rate limits
4. **Sharding**: Consider tenant-based sharding for very large deployments
5. **Read Replicas**: Use for read-heavy workloads

## Adding New Tenants

### Via Database

```sql
INSERT INTO tenants (
    name,
    api_key,
    webhook_secret,
    stellar_account,
    rate_limit_per_minute
) VALUES (
    'New Partner',
    'api_key_new_partner_unique',
    'webhook_secret_unique',
    'GSTELLARACCOUNTADDRESS...',
    100
);
```

### Via API (Future Enhancement)

Add admin endpoints:

- `POST /admin/tenants` - Create tenant
- `PUT /admin/tenants/:id` - Update tenant
- `DELETE /admin/tenants/:id` - Deactivate tenant

## Troubleshooting

### Tenant Not Found Error

**Cause**: Invalid API key or inactive tenant

**Solution**:

```sql
-- Check tenant exists and is active
SELECT * FROM tenants WHERE api_key = 'your_api_key';

-- Activate tenant if needed
UPDATE tenants SET is_active = true WHERE api_key = 'your_api_key';
```

### Cross-Tenant Data Visible

**Cause**: Missing tenant_id filter in query

**Solution**: Review all queries in `src/db/queries.rs` and ensure:

```rust
WHERE tenant_id = $1  // Always present
```

### Performance Issues

**Cause**: Missing indexes on tenant_id

**Solution**:

```sql
-- Check indexes
SELECT * FROM pg_indexes WHERE tablename = 'transactions';

-- Add missing indexes
CREATE INDEX idx_transactions_tenant_id ON transactions(tenant_id);
```

## Future Enhancements

1. **Rate Limiting Middleware**: Enforce per-tenant rate limits
2. **Webhook Signature Validation**: Implement HMAC verification
3. **Admin API**: Tenant management endpoints
4. **Audit Logging**: Track all data access per tenant
5. **Tenant Metrics Dashboard**: Usage statistics per tenant
6. **API Key Rotation**: Automated key rotation mechanism
7. **Multi-Region Support**: Tenant-based geographic routing
8. **Backup/Restore**: Per-tenant backup capabilities

## References

- [Axum Documentation](https://docs.rs/axum/)
- [SQLx Documentation](https://docs.rs/sqlx/)
- [PostgreSQL Row Level Security](https://www.postgresql.org/docs/current/ddl-rowsecurity.html)
- [Multi-Tenancy Patterns](https://docs.microsoft.com/en-us/azure/architecture/patterns/multi-tenancy)
