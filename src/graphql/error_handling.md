//! GraphQL Error Handling Documentation
//!
//! This document describes how errors are handled within the GraphQL module,
//! including error types, propagation, and client-facing error responses.

# GraphQL Error Handling

## Overview

The GraphQL module implements comprehensive error handling to ensure secure, predictable, and user-friendly error responses. This document describes the error handling patterns used throughout the GraphQL schema and resolvers.

## Error Categories

### 1. Validation Errors

Validation errors occur when input parameters fail validation checks. These are typically caused by:
- Invalid data formats (e.g., malformed UUIDs)
- Out-of-range values (e.g., pagination limits exceeding maximum)
- Missing required fields
- Invalid character sets

**Example:**
```graphql
query {
  transactions(limit: 101) {
    id
  }
}
```

**Error Response:**
```json
{
  "errors": [
    {
      "message": "Limit must not exceed 100",
      "path": ["transactions"],
      "extensions": {
        "code": "ERR_VALIDATION_001"
      }
    }
  ]
}
```

All `extensions.code` values are drawn from the shared error catalog
(`docs/error-catalog.md`), the same codes the REST API returns. Resolvers
never build errors by hand; they convert domain errors through
`graphql::error::IntoGraphQlError`, which sets the code and applies the same
redaction as the REST layer.

### 2. Authentication Errors

Authentication errors occur when a request lacks valid credentials. These are handled by the authentication middleware before reaching GraphQL resolvers.

**Example:**
```graphql
query {
  transactions {
    id
  }
}
```

**Error Response (missing auth):**
```json
{
  "errors": [
    {
      "message": "Authentication required",
      "extensions": {
        "code": "ERR_UNAUTHORIZED_001"
      }
    }
  ]
}
```

### 3. Authorization Errors

Authorization errors occur when an authenticated user attempts to access resources they don't have permission to access.

**Example:**
```json
{
  "errors": [
    {
      "message": "Access denied to this resource",
      "extensions": {
        "code": "ERR_AUTH_002"
      }
    }
  ]
}
```

### 4. Resource Not Found Errors

These errors occur when a requested resource doesn't exist in the database.

**Example:**
```graphql
query {
  transaction(id: "550e8400-e29b-41d4-a716-446655440000") {
    id
  }
}
```

**Error Response:**
```json
{
  "errors": [
    {
      "message": "Not found: Resource not found",
      "path": ["transaction"],
      "extensions": {
        "code": "ERR_NOT_FOUND_001"
      }
    }
  ]
}
```

### 5. Database Errors

Database errors occur when there's an issue with the underlying database operations. These are typically logged server-side and a generic error is returned to the client.

**Example:**
```json
{
  "errors": [
    {
      "message": "A database error occurred",
      "extensions": {
        "code": "ERR_DATABASE_001"
      }
    }
  ]
}
```

### 6. Query Complexity Errors

These errors occur when a query exceeds the configured complexity limits (depth, alias count, etc.).

**Example:**
```graphql
query {
  transactions {
    id
    status
    asset_code
    stellar_account
    # ... too many fields or nested queries
  }
}
```

**Error Response:**
```json
{
  "errors": [
    {
      "message": "Query complexity exceeds limit of 1000",
      "extensions": {
        "code": "ERR_QUERY_COMPLEXITY_001"
      }
    }
  ]
}
```

## Error Handling Patterns

### Pattern 1: Convert through the taxonomy, never `?` on a domain error

`async_graphql` has a blanket `From<T: Display>` for its `Error`, so a bare
`?` on an `AppError` or `sqlx::Error` produces a code-less, unredacted error.
Convert explicitly with `.into_gql()` (from `graphql::error::GqlResultExt`):

```rust
use crate::graphql::error::{GqlResultExt, IntoGraphQlError};

async fn transactions(
    &self,
    ctx: &Context<'_>,
    limit: Option<i64>,
) -> Result<Vec<Transaction>> {
    validate_limit(limit.unwrap_or(20)).into_gql()?;

    let state = ctx.data::<AppState>()?;
    queries::list_transactions(&state.db, limit.unwrap_or(20), None, false)
        .await
        .into_gql()
}
```

### Pattern 2: Database Error Mapping

`sqlx::Error` converts through `AppError::from`, so `RowNotFound` becomes
`ERR_NOT_FOUND_001` and every other database error becomes a redacted
`ERR_DATABASE_001`. The raw cause is logged by the conversion, not by the
resolver:

```rust
async fn transaction(&self, ctx: &Context<'_>, id: Uuid) -> Result<Transaction> {
    let state = ctx.data::<AppState>()?;
    queries::get_transaction(&state.db, id).await.into_gql()
}
```

For a hand-built domain error, construct the `AppError` variant and call
`.into_graphql_error()`:

```rust
return Err(AppError::ConcurrentModification(
    "transaction status changed before completion could be applied".to_string(),
)
.into_graphql_error());
```

### Pattern 3: Contextual Error Messages

Provide specific error messages for different failure scenarios:

```rust
pub fn validate_api_key(key: &str) -> Result<(), String> {
    if key.is_empty() {
        return Err("API key cannot be empty".to_string());
    }
    if key.len() < MIN_API_KEY_LENGTH {
        return Err(format!(
            "API key must be at least {} characters long",
            MIN_API_KEY_LENGTH
        ));
    }
    // ...
}
```

## Error Extensions

The GraphQL module uses error extensions to provide additional context:

```json
{
  "errors": [
    {
      "message": "Error message here",
      "extensions": {
        "code": "ERR_CATEGORY_NNN"
      }
    }
  ]
}
```

### Standard Error Codes

Error codes are defined once in `src/error.rs` (`codes` module) and catalogued
in `docs/error-catalog.md`. The REST API returns them in the response body's
`code` field; GraphQL returns the same values in `extensions.code`. The codes
most often seen from GraphQL resolvers:

- `ERR_VALIDATION_001`: Input validation failed
- `ERR_UNAUTHORIZED_001` / `ERR_AUTH_001`: Authentication required or failed
- `ERR_AUTH_002`: Caller lacks permission
- `ERR_NOT_FOUND_001`: Resource not found
- `ERR_DATABASE_001`: Database operation failed (details redacted)
- `ERR_QUERY_COMPLEXITY_001`: Query exceeds depth, complexity, or alias limits
- `ERR_RATE_LIMIT_001`: Rate limit exceeded (carries `retryAfter`)
- `ERR_TRANSACTION_005`: Invalid transaction status transition
- `ERR_TRANSACTION_006`: Concurrent modification during a mutation
- `ERR_INTERNAL_001`: Unexpected server error (details redacted)

## Security Considerations

### 1. Information Disclosure

Never expose sensitive information in error messages:
- Don't return database error details
- Don't expose internal stack traces
- Don't reveal system configuration

### 2. Error Rate Limiting

Implement rate limiting on error responses to prevent error enumeration attacks.

### 3. Logging

Log all errors server-side with appropriate context:
- Error type and code
- Request ID
- User ID (if authenticated)
- Timestamp
- Relevant parameters (sanitized)

## Testing Error Handling

### Unit Tests

Test error conditions in isolation:

```rust
#[test]
fn test_validate_limit_too_high() {
    assert!(validate_limit(Some(101)).is_err());
}
```

### Integration Tests

Test error responses through GraphQL queries:

```rust
#[tokio::test]
async fn test_transaction_not_found() {
    let query = r#"
        query {
            transaction(id: "00000000-0000-0000-0000-000000000000") {
                id
            }
        }
    "#;
    // Execute query and verify error response
}
```

## Best Practices

1. **Validate Early**: Validate inputs before any business logic
2. **Be Specific**: Provide clear, actionable error messages
3. **Log Everything**: Log errors server-side for debugging
4. **Hide Details**: Don't expose internal implementation details
5. **Use Extensions**: Include error codes for programmatic handling
6. **Handle Gracefully**: Always return a response, never panic
7. **Document Errors**: Document possible errors for each field in the schema

## Error Recovery

### Client-Side Recovery

Clients should:
- Check error codes in extensions
- Implement retry logic for transient errors
- Display user-friendly messages based on error type
- Log errors for debugging

### Retry Strategy

For transient errors (database timeouts, network issues):
- Implement exponential backoff
- Limit retry attempts (e.g., 3 retries)
- Only retry idempotent operations

## References

- [GraphQL Error Handling Best Practices](https://graphql.org/learn/validation/)
- [async-graphql Error Documentation](https://docs.rs/async-graphql/)
- [OWASP Error Handling Guidelines](https://owasp.org/www-community/controls/Error_Handling)
