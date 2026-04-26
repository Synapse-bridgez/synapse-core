# Synapse Core - Multi-Tenant Anchor Platform Integration

A multi-tenant Rust/Axum service that supports multiple Anchor Platform integrations on a single deployment with complete tenant isolation.

## Features

- **Tenant Isolation**: Complete data separation using tenant_id with row-level security
- **Flexible Authentication**: Support for API keys, headers, and URL path-based tenant resolution
- **Per-Tenant Configuration**: Individual webhook secrets, Stellar accounts, and rate limits
- **Secure by Default**: All queries enforce tenant_id filtering at the database layer
- **RESTful API**: Clean API design with proper error handling

## Architecture

### Tenant Resolution

The system identifies tenants through multiple methods (in order of precedence):

1. **URL Path**: `/api/transactions` with tenant context from other methods
2. **API Key Header**: `X-API-Key: your_api_key` or `Authorization: Bearer your_api_key`
3. **Tenant ID Header**: `X-Tenant-ID: uuid`

### Data Isolation

- Every transaction includes a `tenant_id` foreign key
- All database queries include `WHERE tenant_id = $1` filters
- PostgreSQL Row Level Security (RLS) enabled as additional safeguard
- Unique constraints scoped per tenant (e.g., external_id)

### Security Model

```
Request → Tenant Resolution → TenantContext Extractor → Handler
                                      ↓
                              Validates tenant exists
                              Checks tenant is_active
                              Loads tenant config
                                      ↓
                              All queries filtered by tenant_id
```

## API Endpoints

All endpoints require tenant authentication via API key or tenant ID header.

### Transactions

- `POST /api/transactions` - Create a new transaction
- `GET /api/transactions` - List transactions (paginated)
- `GET /api/transactions/:id` - Get specific transaction
- `PUT /api/transactions/:id` - Update transaction
- `DELETE /api/transactions/:id` - Delete transaction

### Webhooks

- `POST /api/webhook` - Receive webhook events

## Database Schema

### Tenants Table

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

### Transactions Table

```sql
CREATE TABLE transactions (
    transaction_id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL REFERENCES tenants(tenant_id),
    external_id VARCHAR(255) NOT NULL,
    status VARCHAR(50) NOT NULL DEFAULT 'pending',
    amount VARCHAR(50) NOT NULL,
    asset_code VARCHAR(12) NOT NULL,
    stellar_transaction_id VARCHAR(64),
    memo TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT unique_external_id_per_tenant UNIQUE (tenant_id, external_id)
);
```

## Setup

1. Install Rust and PostgreSQL
2. Copy `.env.example` to `.env` and configure database URL
3. Run migrations:
   ```bash
   psql $DATABASE_URL -f migrations/001_create_tenants.sql
   psql $DATABASE_URL -f migrations/002_create_transactions.sql
   psql $DATABASE_URL -f migrations/003_seed_sample_tenants.sql
   ```
4. Build and run:
   ```bash
   cargo build --release
   cargo run
   ```

## Usage Examples

### Create Transaction

```bash
curl -X POST http://localhost:3000/api/transactions \
  -H "X-API-Key: demo_api_key_anchor_platform_001" \
  -H "Content-Type: application/json" \
  -d '{
    "external_id": "ext_12345",
    "amount": "100.50",
    "asset_code": "USDC",
    "memo": "Payment for services"
  }'
```

### List Transactions

```bash
curl http://localhost:3000/api/transactions?limit=10&offset=0 \
  -H "X-API-Key: demo_api_key_anchor_platform_001"
```

### Get Transaction

```bash
curl http://localhost:3000/api/transactions/{transaction_id} \
  -H "X-API-Key: demo_api_key_anchor_platform_001"
```

## Testing Tenant Isolation

The seed data includes three tenants:

1. **Anchor Platform Demo** - Active tenant (API Key: `demo_api_key_anchor_platform_001`)
2. **Partner Integration Test** - Active tenant (API Key: `test_api_key_partner_002`)
3. **Inactive Tenant** - Inactive (will reject requests)

Test isolation by:

1. Creating transactions with different API keys
2. Verifying each tenant can only see their own transactions
3. Confirming inactive tenant requests are rejected

## Security Considerations

- **Query-Level Filtering**: Never rely on application logic alone - always filter by tenant_id in SQL
- **API Key Storage**: Store API keys securely (consider hashing in production)
- **Webhook Secrets**: Validate webhook signatures using per-tenant secrets
- **Rate Limiting**: Implement per-tenant rate limits (configured in tenant table)
- **Audit Logging**: Consider adding audit logs for compliance

## Development

Run tests:

```bash
cargo test
```

Run with debug logging:

```bash
RUST_LOG=debug cargo run
```

## Production Considerations

1. **Database Connection Pooling**: Adjust `max_connections` based on load
2. **Rate Limiting**: Implement middleware using `rate_limit_per_minute` from tenant config
3. **Webhook Signature Validation**: Add HMAC validation using `webhook_secret`
4. **Monitoring**: Add metrics for per-tenant usage
5. **Backup Strategy**: Ensure tenant data can be backed up/restored independently
6. **API Key Rotation**: Implement key rotation mechanism
7. **Audit Trail**: Log all tenant data access for compliance

## License

MIT
