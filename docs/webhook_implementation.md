# Webhook Handler Implementation

## Overview

This document describes the implementation of the `POST /callback/transaction` endpoint that receives fiat deposit events from the Stellar Anchor Platform.

## Endpoint

```
POST /callback/transaction
```

## Request Payload

```json
{
  "id": "anchor-tx-12345",
  "amount_in": "100.50",
  "stellar_account": "GABCDEFGHIJKLMNOPQRSTUVWXYZ234567890ABCDEFGHIJKLMNOPQR",
  "asset_code": "USD"
}
```

### Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `id` | string | Yes | Unique transaction ID from the Anchor Platform |
| `amount_in` | string | Yes | Deposit amount (must be > 0) |
| `stellar_account` | string | Yes | Stellar public key (56 chars, starts with 'G') |
| `asset_code` | string | Yes | Asset code (1-12 chars, e.g., "USD", "USDC") |

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

## Validation Rules

The endpoint validates the following business rules:

1. **Amount Validation**
   - Must be a valid decimal number
   - Must be greater than 0
   - Supports arbitrary precision via `BigDecimal`

2. **Stellar Account Validation**
   - Must be exactly 56 characters long
   - Must start with 'G' (Stellar public key prefix)

3. **Asset Code Validation**
   - Must not be empty
   - Must be at most 12 characters (Stellar asset code limit)

## Database Persistence

The transaction is persisted to the `transactions` table with the following fields:

- `id`: Auto-generated UUID
- `stellar_account`: From payload
- `amount`: Parsed from `amount_in`
- `asset_code`: From payload
- `status`: Set to "pending"
- `created_at`: Current timestamp
- `updated_at`: Current timestamp
- `anchor_transaction_id`: From payload `id`
- `callback_type`: Set to "deposit"
- `callback_status`: Set to "pending"

## Implementation Details

### Files Modified

1. **src/handlers/webhook.rs** (new)
   - `CallbackPayload` struct for request deserialization
   - `CallbackResponse` struct for response serialization
   - `validate_payload()` function for business rule validation
   - `handle_callback()` async handler function

2. **src/handlers/mod.rs**
   - Added `pub mod webhook;` export

3. **src/main.rs**
   - Imported `post` from `axum::routing`
   - Registered route: `.route("/callback/transaction", post(handlers::webhook::handle_callback))`

### Error Handling

The handler uses the existing `AppError` enum from `src/error.rs`:

- `AppError::Validation` - For business rule violations (400)
- `AppError::Database` - For database errors (500)

All errors are automatically converted to JSON responses with appropriate HTTP status codes via the `IntoResponse` implementation.

## Testing

### Unit Tests

The implementation includes comprehensive unit tests in `src/handlers/webhook.rs`:

- `test_validate_payload_valid` - Valid payload passes validation
- `test_validate_payload_zero_amount` - Zero amount rejected
- `test_validate_payload_negative_amount` - Negative amount rejected
- `test_validate_payload_invalid_stellar_account_length` - Invalid length rejected
- `test_validate_payload_invalid_stellar_account_prefix` - Invalid prefix rejected
- `test_validate_payload_empty_asset_code` - Empty asset code rejected
- `test_validate_payload_asset_code_too_long` - Long asset code rejected

Run tests with:
```bash
cargo test
```

### Integration Testing

Use the provided `test_webhook.sh` script to test the endpoint manually:

```bash
# Start the server
docker-compose up

# In another terminal, run the test script
./test_webhook.sh http://localhost:3000
```

The script tests:
1. Valid deposit callback (expects 201)
2. Invalid amount - zero (expects 400)
3. Invalid Stellar account - too short (expects 400)
4. Invalid Stellar account - wrong prefix (expects 400)
5. Empty asset code (expects 400)
6. Large amount deposit (expects 201)

### Manual Testing with curl

```bash
# Valid request
curl -X POST http://localhost:3000/callback/transaction \
  -H "Content-Type: application/json" \
  -d '{
    "id": "anchor-tx-12345",
    "amount_in": "100.50",
    "stellar_account": "GABCDEFGHIJKLMNOPQRSTUVWXYZ234567890ABCDEFGHIJKLMNOPQR",
    "asset_code": "USD"
  }'

# Invalid request (zero amount)
curl -X POST http://localhost:3000/callback/transaction \
  -H "Content-Type: application/json" \
  -d '{
    "id": "anchor-tx-12346",
    "amount_in": "0",
    "stellar_account": "GABCDEFGHIJKLMNOPQRSTUVWXYZ234567890ABCDEFGHIJKLMNOPQR",
    "asset_code": "USD"
  }'
```

## Security Considerations

1. **Input Validation**: All inputs are validated before database insertion
2. **SQL Injection**: Using SQLx parameterized queries prevents SQL injection
3. **Type Safety**: Rust's type system ensures type safety at compile time
4. **Error Handling**: Errors don't leak sensitive information to clients

## Future Enhancements

1. **Authentication**: Add webhook signature verification
2. **Idempotency**: Prevent duplicate transaction processing
3. **Rate Limiting**: Protect against abuse
4. **Async Processing**: Move heavy processing to background jobs
5. **Stellar Verification**: Verify the transaction on-chain before accepting

## References

- [Stellar Anchor Platform Documentation](https://github.com/stellar/anchor-platform)
- [Stellar Account Format](https://developers.stellar.org/docs/fundamentals-and-concepts/stellar-data-structures/accounts)
- [Architecture Documentation](./architecture.md)
