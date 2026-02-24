# Quick Start Guide

Get Synapse Core running with multi-tenant support in 5 minutes.

## Prerequisites

- Rust 1.70+ (`rustup install stable`)
- PostgreSQL 14+ (`brew install postgresql` or `apt install postgresql`)
- curl (for testing)

## Step 1: Setup Database

```bash
# Create database
createdb synapse_core

# Or using psql
psql -U postgres -c "CREATE DATABASE synapse_core;"
```

## Step 2: Configure Environment

```bash
# Copy example environment file
cp .env.example .env

# Edit .env with your database credentials
# DATABASE_URL=postgresql://username:password@localhost:5432/synapse_core
```

## Step 3: Run Migrations

```bash
# Make script executable
chmod +x run_migrations.sh

# Run migrations
./run_migrations.sh
```

Expected output:

```
Running migrations...
Applying migrations/001_create_tenants.sql...
Applying migrations/002_create_transactions.sql...
Applying migrations/003_seed_sample_tenants.sql...
All migrations completed successfully!
```

## Step 4: Build and Run

```bash
# Build the project
cargo build --release

# Run the server
cargo run
```

Expected output:

```
2024-01-15T10:00:00.000Z  INFO synapse_core: Listening on 0.0.0.0:3000
```

## Step 5: Test the API

### Test Tenant 1 (Anchor Platform Demo)

```bash
# Create a transaction
curl -X POST http://localhost:3000/api/transactions \
  -H "X-API-Key: demo_api_key_anchor_platform_001" \
  -H "Content-Type: application/json" \
  -d '{
    "external_id": "demo_tx_001",
    "amount": "100.50",
    "asset_code": "USDC",
    "memo": "Test transaction"
  }'
```

Expected response:

```json
{
  "transaction": {
    "transaction_id": "uuid-here",
    "tenant_id": "11111111-1111-1111-1111-111111111111",
    "external_id": "demo_tx_001",
    "status": "pending",
    "amount": "100.50",
    "asset_code": "USDC",
    "stellar_transaction_id": null,
    "memo": "Test transaction",
    "created_at": "2024-01-15T10:00:00Z",
    "updated_at": "2024-01-15T10:00:00Z"
  }
}
```

### List Transactions

```bash
curl http://localhost:3000/api/transactions \
  -H "X-API-Key: demo_api_key_anchor_platform_001"
```

### Test Tenant 2 (Partner Integration)

```bash
# Create transaction for different tenant
curl -X POST http://localhost:3000/api/transactions \
  -H "X-API-Key: test_api_key_partner_002" \
  -H "Content-Type: application/json" \
  -d '{
    "external_id": "partner_tx_001",
    "amount": "250.00",
    "asset_code": "USDC"
  }'
```

### Verify Tenant Isolation

```bash
# List transactions for Tenant 1
curl http://localhost:3000/api/transactions \
  -H "X-API-Key: demo_api_key_anchor_platform_001"
# Should only show demo_tx_001

# List transactions for Tenant 2
curl http://localhost:3000/api/transactions \
  -H "X-API-Key: test_api_key_partner_002"
# Should only show partner_tx_001
```

### Test Inactive Tenant (Should Fail)

```bash
curl http://localhost:3000/api/transactions \
  -H "X-API-Key: inactive_api_key_003"
```

Expected response:

```json
{
  "error": "Unauthorized"
}
```

## Step 6: Verify Database Isolation

```bash
# Connect to database
psql $DATABASE_URL

# Check tenants
SELECT tenant_id, name, is_active FROM tenants;

# Check transactions per tenant
SELECT tenant_id, COUNT(*) as transaction_count
FROM transactions
GROUP BY tenant_id;

# Verify isolation - this query simulates what the API does
SELECT * FROM transactions
WHERE tenant_id = '11111111-1111-1111-1111-111111111111';
```

## Common Operations

### Update Transaction Status

```bash
curl -X PUT http://localhost:3000/api/transactions/{transaction_id} \
  -H "X-API-Key: demo_api_key_anchor_platform_001" \
  -H "Content-Type: application/json" \
  -d '{
    "status": "completed",
    "stellar_transaction_id": "stellar_hash_here"
  }'
```

### Delete Transaction

```bash
curl -X DELETE http://localhost:3000/api/transactions/{transaction_id} \
  -H "X-API-Key: demo_api_key_anchor_platform_001"
```

### Send Webhook

```bash
curl -X POST http://localhost:3000/api/webhook \
  -H "X-API-Key: demo_api_key_anchor_platform_001" \
  -H "Content-Type: application/json" \
  -d '{
    "event_type": "transaction.completed",
    "transaction_id": "tx_12345",
    "data": {
      "status": "completed"
    }
  }'
```

## Adding a New Tenant

```bash
# Connect to database
psql $DATABASE_URL

# Insert new tenant
INSERT INTO tenants (
    name,
    api_key,
    webhook_secret,
    stellar_account,
    rate_limit_per_minute
) VALUES (
    'My New Partner',
    'api_key_my_new_partner_unique',
    'webhook_secret_unique_value',
    'GSTELLARACCOUNTADDRESSHERE...',
    100
);

# Verify
SELECT * FROM tenants WHERE name = 'My New Partner';
```

## Troubleshooting

### "Connection refused" error

- Check PostgreSQL is running: `pg_isready`
- Verify DATABASE_URL in .env file
- Check PostgreSQL is listening: `psql -U postgres -c "SHOW port;"`

### "Unauthorized" error

- Verify API key is correct
- Check tenant is active: `SELECT is_active FROM tenants WHERE api_key = 'your_key';`
- Ensure you're using the correct header: `X-API-Key` or `Authorization: Bearer`

### "Transaction not found" error

- Verify you're using the correct tenant API key
- Check transaction exists: `SELECT * FROM transactions WHERE transaction_id = 'uuid';`
- Confirm transaction belongs to your tenant

### Migrations fail

- Check database exists: `psql -l | grep synapse_core`
- Verify database permissions
- Check PostgreSQL version: `psql --version` (need 14+)

## Next Steps

1. Read [IMPLEMENTATION_GUIDE.md](IMPLEMENTATION_GUIDE.md) for architecture details
2. Review [README.md](README.md) for full API documentation
3. Implement webhook signature validation
4. Add rate limiting middleware
5. Set up monitoring and logging
6. Configure production deployment

## Development Tips

### Run with debug logging

```bash
RUST_LOG=debug cargo run
```

### Watch mode (auto-reload on changes)

```bash
cargo install cargo-watch
cargo watch -x run
```

### Format code

```bash
cargo fmt
```

### Run linter

```bash
cargo clippy
```

### Run tests

```bash
cargo test
```

## Production Checklist

- [ ] Use strong, unique API keys for each tenant
- [ ] Enable HTTPS/TLS
- [ ] Implement rate limiting
- [ ] Add webhook signature validation
- [ ] Set up monitoring and alerting
- [ ] Configure database backups
- [ ] Review and tune connection pool settings
- [ ] Enable audit logging
- [ ] Set up log aggregation
- [ ] Configure firewall rules
- [ ] Review security headers
- [ ] Implement API key rotation policy

## Support

For issues or questions:

1. Check the [IMPLEMENTATION_GUIDE.md](IMPLEMENTATION_GUIDE.md)
2. Review error logs: `RUST_LOG=debug cargo run`
3. Verify database state with SQL queries
4. Check PostgreSQL logs

Happy coding! 🚀
