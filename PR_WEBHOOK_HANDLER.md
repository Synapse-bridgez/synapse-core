# Pull Request: Implement Webhook Handler for Fiat Deposit Events

## Description

This PR implements the core feature of Phase 1: the `POST /callback/transaction` endpoint to receive and persist fiat deposit events from the Stellar Anchor Platform.

## Changes

### Core Implementation

1. **Webhook Handler** (`src/handlers/webhook.rs`)
   - Added `CallbackPayload` struct with serde deserialization
   - Added `CallbackResponse` struct for API responses
   - Implemented `validate_payload()` function with business rules
   - Implemented `handle_callback()` handler function

2. **Route Registration** (`src/main.rs`)
   - Registered `POST /callback/transaction` route

### Validation Rules

The handler validates:
- ✅ Amount must be greater than 0
- ✅ Stellar account must be exactly 56 characters
- ✅ Stellar account must start with 'G' (public key prefix)
- ✅ Asset code must be between 1-12 characters

### Database Persistence

Transactions are persisted with:
- Auto-generated UUID
- Status set to "pending"
- All callback metadata (anchor_transaction_id, callback_type, callback_status)
- Timestamps (created_at, updated_at)

### Testing

1. **Integration Tests** (`tests/webhook_test.rs`)
   - Valid payload test
   - Invalid amount validation test
   - Invalid Stellar account validation test
   - Invalid asset code validation test

2. **Manual Testing Script** (`test-callback.sh`)
   - 5 test cases covering success and error scenarios
   - Easy to run: `./test-callback.sh`

### Documentation

- Added comprehensive documentation in `docs/webhook-handler.md`
- Includes API specification, validation rules, testing guide, and implementation details

## API Example

### Request

```bash
curl -X POST http://localhost:3000/callback/transaction \
  -H "Content-Type: application/json" \
  -d '{
    "id": "anchor-tx-12345",
    "amount_in": "100.50",
    "stellar_account": "GABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890ABCDEFGHIJKLMNOP",
    "asset_code": "USD",
    "callback_type": "deposit",
    "status": "completed"
  }'
```

### Response (201 Created)

```json
{
  "transaction_id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "pending"
}
```

### Error Response (400 Bad Request)

```json
{
  "error": "Validation error: Amount must be greater than 0",
  "status": 400
}
```

## Testing Instructions

1. Start the application:
   ```bash
   docker-compose up
   ```

2. Run the test script:
   ```bash
   ./test-callback.sh
   ```

3. Run integration tests:
   ```bash
   cargo test --test webhook_test
   ```

4. Verify database persistence:
   ```bash
   docker-compose exec postgres psql -U user -d synapse -c "SELECT * FROM transactions;"
   ```

## Checklist

- [x] Feature branch created: `feature/issue-2-webhook-handler`
- [x] `CallbackPayload` struct defined with serde
- [x] Validation logic implemented
- [x] Database insert with sqlx
- [x] Route registered in main.rs
- [x] Integration tests added
- [x] Manual testing script created
- [x] Documentation written
- [x] Error handling with AppError
- [x] Logging added
- [x] Returns 201 Created on success

## Related Issues

Closes #2

## Next Steps

After this PR is merged:
1. Add idempotency middleware to prevent duplicate processing
2. Implement transaction processor for status updates
3. Add Stellar on-chain verification
4. Add webhook signature verification for security

## References

- Architecture documentation: `docs/architecture.md`
- Database schema: `migrations/20250216000000_init.sql`
- Error handling: `src/error.rs`
