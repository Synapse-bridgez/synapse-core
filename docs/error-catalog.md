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

### Rate Limiting Errors (ERR_RATE_LIMIT_xxx)

| Code | HTTP Status | Description |
|------|-------------|-------------|
| ERR_RATE_LIMIT_001 | 429 | Rate limit exceeded |

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
