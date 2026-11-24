# Error Catalog

This document lists all stable error codes used by the Synapse Core API. Error codes are stable and should never be renamed or reused for different errors.

## Error Response Format

All error responses follow this format:

```json
{
  "error": "Human readable error message",
  "code": "ERR_CATEGORY_NNN",
  "status": 400
}
```

## Error Codes

### Database Errors (ERR_DATABASE_xxx)

| Code | HTTP Status | Description |
|------|-------------|-------------|
| ERR_DATABASE_001 | 500 | Database connection error |
| ERR_DATABASE_002 | 500 | Database query execution error |

### Validation Errors (ERR_VALIDATION_xxx)

| Code | HTTP Status | Description |
|------|-------------|-------------|
| ERR_VALIDATION_001 | 400 | Validation error - invalid input |

### Not Found Errors (ERR_NOT_FOUND_xxx)

| Code | HTTP Status | Description |
|------|-------------|-------------|
| ERR_NOT_FOUND_001 | 404 | Resource not found |

### Internal Errors (ERR_INTERNAL_xxx)

| Code | HTTP Status | Description |
|------|-------------|-------------|
| ERR_INTERNAL_001 | 500 | Internal server error |

### Bad Request Errors (ERR_BAD_REQUEST_xxx)

| Code | HTTP Status | Description |
|------|-------------|-------------|
| ERR_BAD_REQUEST_001 | 400 | Bad request - invalid parameters |

### Authentication Errors (ERR_AUTH_xxx)

| Code | HTTP Status | Description |
|------|-------------|-------------|
| ERR_AUTH_001 | 401 | Invalid authentication credentials |
| ERR_AUTH_002 | 403 | Insufficient permissions |

### Unauthorized Errors (ERR_UNAUTHORIZED_xxx)

| Code | HTTP Status | Description |
|------|-------------|-------------|
| ERR_UNAUTHORIZED_001 | 401 | Unauthorized - authentication required |

### Transaction Errors (ERR_TRANSACTION_xxx)

| Code | HTTP Status | Description |
|------|-------------|-------------|
| ERR_TRANSACTION_001 | 400 | Invalid transaction amount |
| ERR_TRANSACTION_002 | 400 | Transaction amount below minimum |
| ERR_TRANSACTION_003 | 400 | Invalid Stellar address |
| ERR_TRANSACTION_004 | 409 | Transaction already processed (idempotency) |
| ERR_TRANSACTION_005 | 400 | Invalid transaction status transition |

### Webhook Errors (ERR_WEBHOOK_xxx)

| Code | HTTP Status | Description |
|------|-------------|-------------|
| ERR_WEBHOOK_001 | 401 | Invalid webhook signature |
| ERR_WEBHOOK_002 | 400 | Malformed webhook payload |

### Settlement Errors (ERR_SETTLEMENT_xxx)

| Code | HTTP Status | Description |
|------|-------------|-------------|
| ERR_SETTLEMENT_001 | 400 | Invalid settlement amount |
| ERR_SETTLEMENT_002 | 409 | Settlement already exists |
| ERR_SETTLEMENT_003 | 409 | Stale transition: settlement state changed during processing |

### Rate Limiting Errors (ERR_RATE_LIMIT_xxx)

| Code | HTTP Status | Description |
|------|-------------|-------------|
| ERR_RATE_LIMIT_001 | 429 | Rate limit exceeded |

### Redis Errors (ERR_REDIS_xxx)

| Code | HTTP Status | Description |
|------|-------------|-------------|
| ERR_REDIS_001 | 500 | Redis operation failed |

## Error Variants

Every variant of `AppError` in `src/error.rs` and the code and status it produces. Several variants share a code; the `error` message and `detail` still differ.

| Variant | Code | HTTP Status | Cause |
|---------|------|-------------|-------|
| `Database(sqlx::Error)` | ERR_DATABASE_001 | 500 | A database call failed. `sqlx::Error::RowNotFound` is converted to `NotFound` instead. The raw cause is logged, not returned. |
| `DatabaseError(String)` | ERR_DATABASE_002 | 500 | A database error reported as a string. The raw text is logged, not returned. |
| `Validation(String)` | ERR_VALIDATION_001 | 400 | Request input failed validation. |
| `NotFound(String)` | ERR_NOT_FOUND_001 | 404 | The requested resource does not exist. |
| `Internal(String)` | ERR_INTERNAL_001 | 500 | An unexpected server-side failure. |
| `BadRequest(String)` | ERR_BAD_REQUEST_001 | 400 | A request parameter is invalid. |
| `Unauthorized(String)` | ERR_UNAUTHORIZED_001 | 401 | Authentication is required. |
| `TenantNotFound` | ERR_NOT_FOUND_001 | 404 | The tenant in the request does not exist. |
| `InvalidApiKey` | ERR_UNAUTHORIZED_001 | 401 | The API key or tenant header is invalid. |
| `InvalidTransactionAmount(String)` | ERR_TRANSACTION_001 | 400 | The transaction amount is not valid. |
| `AmountBelowMinimum(String)` | ERR_TRANSACTION_002 | 400 | The amount is below the configured minimum. |
| `InvalidStellarAddress(String)` | ERR_TRANSACTION_003 | 400 | The Stellar address is malformed. |
| `TransactionAlreadyProcessed(String)` | ERR_TRANSACTION_004 | 409 | The transaction was already processed (idempotency). |
| `InvalidStatusTransition(String)` | ERR_TRANSACTION_005 | 400 | The status change is not allowed by the state machine. |
| `StaleTransition` | ERR_SETTLEMENT_003 | 409 | The settlement state changed while the transition was being processed. |
| `InvalidWebhookSignature` | ERR_WEBHOOK_001 | 401 | The webhook signature did not verify. |
| `MalformedWebhookPayload(String)` | ERR_WEBHOOK_002 | 400 | The webhook body could not be parsed. |
| `InvalidSettlementAmount(String)` | ERR_SETTLEMENT_001 | 400 | The settlement amount is not valid. |
| `SettlementAlreadyExists(String)` | ERR_SETTLEMENT_002 | 409 | A settlement already exists for this request. |
| `RateLimitExceeded` | ERR_RATE_LIMIT_001 | 429 | The caller exceeded the rate limit. |
| `AuthenticationFailed(String)` | ERR_AUTH_001 | 401 | The credentials could not be authenticated. |
| `InsufficientPermissions(String)` | ERR_AUTH_002 | 403 | The caller is authenticated but not allowed to do this. |
| `Redis(redis::RedisError)` | ERR_REDIS_001 | 500 | A Redis operation failed. The raw cause is logged, not returned. |
| `Anyhow(anyhow::Error)` | ERR_INTERNAL_001 | 500 | An uncategorised internal error. |

## Using Error Codes

### Programmatic Retry Logic

Clients can use error codes to implement intelligent retry logic:

```python
# Example: Retry on transient errors
TRANSIENT_ERRORS = ["ERR_DATABASE_001", "ERR_INTERNAL_001"]

def handle_error(response):
    error_code = response["code"]
    if error_code in TRANSIENT_ERRORS:
        # Retry with exponential backoff
        return retry_with_backoff()
    elif error_code == "ERR_RATE_LIMIT_001":
        # Retry after waiting for rate limit reset
        return retry_after_delay()
    else:
        # Don't retry for client errors
        return handle_failure(response)
```

### Idempotency Handling

Clients can detect idempotent operations that have already been processed:

```python
if response["code"] == "ERR_TRANSACTION_004":
    # Transaction was already processed
    return get_existing_result()
```

## Version

This error catalog is version 1.0.0. API consumers can retrieve the latest version via the `/errors` endpoint.

## Changelog

- 1.0.0 - Initial error catalog with 19 error codes
- Unreleased - Documented `ERR_SETTLEMENT_003` and `ERR_REDIS_001`, which `src/error.rs` already defines, and added the table of `AppError` variants
