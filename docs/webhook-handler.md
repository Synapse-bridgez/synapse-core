# Webhook Handler Implementation

## Overview

The webhook handler implements the `POST /callback/transaction` endpoint to receive fiat deposit events from the Stellar Anchor Platform.

## Endpoint

```
POST /callback/transaction
```

## Request Payload

```json
{
  "id": "anchor-tx-12345",
  "amount_in": "100.50",
  "stellar_account": "GABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890ABCDEFGHIJKLMNOP",
  "asset_code": "USD",
  "callback_type": "deposit",
  "status": "completed"
}
```

### Required Fields

- `id` (string): Anchor Platform transaction ID
- `amount_in` (string): Deposit amount as a decimal string
- `stellar_account` (string): Stellar public key (56 characters, starts with 'G')
- `asset_code` (string): Asset code (1-12 characters, e.g., "USD")

### Optional Fields

- `callback_type` (string): Type of callback (e.g., "deposit", "withdrawal")
- `status` (string): Original status from the Anchor Platform

## Validation Rules

The handler validates the following business rules:

1. **Amount validation**
   - Must be a valid decimal number
   - Must be greater than 0

2. **Stellar account validation**
   - Must be exactly 56 characters long
   - Must start with 'G' (public key prefix)

3. **Asset code validation**
   - Must not be empty
   - Must be 12 characters or less (per Stellar specification)

## Response

### Success (201 Created)

```json
{
  "transaction_id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "pending"
}
```

### Error (400 Bad Request)

```json
{
  "error": "Validation error: Amount must be greater than 0",
  "status": 400
}
```

## Database Persistence

The handler creates a new transaction record with:

- `id`: Auto-generated UUID
- `stellar_account`: From payload
- `amount`: Parsed from `amount_in`
- `asset_code`: From payload
- `status`: Set to "pending"
- `anchor_transaction_id`: From payload `id`
- `callback_type`: From payload (optional)
- `callback_status`: From payload `status` (optional)
- `created_at`: Current timestamp
- `updated_at`: Current timestamp

## Testing

### Manual Testing with curl

Run the test script:

```bash
./test-callback.sh
```

Or manually:

```bash
curl -X POST http://localhost:3000/callback/transaction \
  -H "Content-Type: application/json" \
  -d '{
    "id": "anchor-tx-12345",
    "amount_in": "100.50",
    "stellar_account": "GABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890ABCDEFGHIJKLMNOP",
    "asset_code": "USD"
  }'
```

### Integration Tests

Run the test suite:

```bash
cargo test --test webhook_test
```

## Implementation Details

### Files Modified

1. `src/handlers/webhook.rs`
   - Added `CallbackPayload` struct
   - Added `CallbackResponse` struct
   - Implemented `validate_payload()` function
   - Implemented `handle_callback()` handler

2. `src/main.rs`
   - Registered `POST /callback/transaction` route

3. `tests/webhook_test.rs` (new)
   - Added validation tests

4. `test-callback.sh` (new)
   - Added manual testing script

## Error Handling

The handler uses the centralized `AppError` enum from `src/error.rs`:

- `AppError::Validation`: For business rule violations (400 Bad Request)
- `AppError::Database`: For database errors (500 Internal Server Error)

All errors are automatically converted to JSON responses with appropriate HTTP status codes.

## Logging

The handler logs:

- Incoming callback with transaction ID, amount, and asset code
- Successful persistence with transaction ID and status

Example log output:

```
INFO Received callback for transaction anchor-tx-12345 with amount 100.50 USD
INFO Transaction 550e8400-e29b-41d4-a716-446655440000 persisted with status: pending
```

## Next Steps

1. Add idempotency middleware to prevent duplicate processing
2. Implement transaction processor to update status
3. Add Stellar on-chain verification
4. Add webhook signature verification for security
