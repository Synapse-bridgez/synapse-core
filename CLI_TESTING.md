# Admin CLI Testing Guide

## Implementation Summary

The admin CLI has been successfully implemented with the following components:

### Files Modified/Created:
1. **src/cli.rs** (new) - CLI command definitions and handlers
2. **src/main.rs** (refactored) - Now uses clap for command parsing
3. **Cargo.toml** (updated) - Added clap dependency

### Available Commands:

#### 1. `serve` - Start HTTP Server (Default)
```bash
cargo run -- serve
# Or simply:
cargo run
```

#### 2. `tx force-complete` - Force Complete a Transaction
```bash
cargo run -- tx force-complete <TX_ID>
# Example:
cargo run -- tx force-complete 550e8400-e29b-41d4-a716-446655440000
```

#### 3. `db migrate` - Run Database Migrations
```bash
cargo run -- db migrate
```

#### 4. `config` - Validate Configuration
```bash
cargo run -- config
```

## Testing Steps

### Prerequisites
```bash
# Ensure PostgreSQL is running
docker run --name synapse-postgres \
  -e POSTGRES_USER=synapse \
  -e POSTGRES_PASSWORD=synapse \
  -e POSTGRES_DB=synapse \
  -p 5432:5432 -d postgres:14-alpine

# Set up environment variables
cp .env.example .env
# Edit .env with correct values
```

### Test 1: Build and Check
```bash
cargo check
cargo build
```

### Test 2: Config Validation
```bash
cargo run -- config
```
**Expected Output:**
```
Configuration:
  Server Port: 3000
  Database URL: postgres://synapse:****@localhost:5432/synapse
  Stellar Horizon URL: https://horizon-testnet.stellar.org
✓ Configuration is valid
```

### Test 3: Database Migration
```bash
cargo run -- db migrate
```
**Expected Output:**
```
✓ Database migrations completed
```

### Test 4: Start Server
```bash
cargo run -- serve
```
**Expected Output:**
```
Database migrations completed
Stellar Horizon client initialized with URL: https://horizon-testnet.stellar.org
listening on 0.0.0.0:3000
```

### Test 5: Force Complete Transaction

First, insert a test transaction:
```bash
docker exec -it synapse-postgres psql -U synapse -d synapse -c "
INSERT INTO transactions (id, stellar_account, amount, asset_code, status)
VALUES ('550e8400-e29b-41d4-a716-446655440000', 'GABCD1234', 100.50, 'USD', 'pending')
RETURNING id, status;
"
```

Then force complete it:
```bash
cargo run -- tx force-complete 550e8400-e29b-41d4-a716-446655440000
```

**Expected Output:**
```
✓ Transaction 550e8400-e29b-41d4-a716-446655440000 marked as completed
```

Verify:
```bash
docker exec -it synapse-postgres psql -U synapse -d synapse -c "
SELECT id, status, updated_at FROM transactions 
WHERE id = '550e8400-e29b-41d4-a716-446655440000';
"
```

### Test 6: Help Commands
```bash
# Main help
cargo run -- --help

# Subcommand help
cargo run -- tx --help
cargo run -- db --help
```

## Error Handling Tests

### Test Invalid Transaction ID
```bash
cargo run -- tx force-complete 00000000-0000-0000-0000-000000000000
```
**Expected:** Error message indicating transaction not found

### Test Invalid UUID Format
```bash
cargo run -- tx force-complete invalid-uuid
```
**Expected:** Clap parsing error

### Test Missing Environment Variables
```bash
unset DATABASE_URL
cargo run -- config
```
**Expected:** Error about missing DATABASE_URL

## Integration with Existing Code

The CLI implementation:
- ✅ Uses the same `Config` struct from `config.rs`
- ✅ Uses the same `PgPool` from `db::create_pool()`
- ✅ Maintains backward compatibility (default behavior is `serve`)
- ✅ Shares logging configuration
- ✅ No breaking changes to existing API

## Security Considerations

1. **Password Masking**: The `config` command masks database passwords in output
2. **Direct DB Access**: `tx force-complete` requires direct database access (secure shell only)
3. **No API Exposure**: Admin commands are CLI-only, not exposed via HTTP

## Next Steps

After testing, you can:
1. Add more transaction management commands (e.g., `tx list`, `tx status`)
2. Add database backup/restore commands
3. Add secret injection commands for production deployments
4. Create shell scripts for common admin tasks

## Troubleshooting

### Issue: Command not recognized
**Solution:** Ensure you're using `--` to separate cargo args from binary args:
```bash
cargo run -- tx force-complete <id>
```

### Issue: Database connection fails
**Solution:** Check DATABASE_URL in .env and ensure PostgreSQL is running

### Issue: Migration fails
**Solution:** Ensure migrations directory exists and contains valid SQL files
