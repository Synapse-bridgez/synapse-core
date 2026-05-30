//! Idempotency Keys Documentation
//!
//! This document describes the idempotency key mechanism used in the authentication module
//! to ensure safe retry of operations without duplicate side effects.

# Idempotency Keys in Authentication

## Overview

Idempotency keys are unique identifiers that allow clients to safely retry operations without causing duplicate side effects. When a client sends a request with an idempotency key, the server checks if a request with that key has already been processed. If it has, the server returns the cached result instead of re-executing the operation.

## Purpose

Idempotency keys solve several critical problems:

1. **Network Failures**: Clients can safely retry requests that failed due to network issues
2. **Timeout Handling**: Operations that timeout can be retried without duplicate execution
3. **Duplicate Prevention**: Accidental duplicate requests (e.g., double-click) are prevented
4. **Consistency**: Ensures the same operation always produces the same result when retried

## How It Works

### Request Flow

```
Client Request with Idempotency Key
         ↓
Check if key exists in cache
         ↓
    ┌────┴────┐
    │         │
  Exists    New Key
    │         │
    ↓         ↓
Return    Execute
Cached   Operation
Result       ↓
         Cache Result
              ↓
         Return Response
```

### Key Requirements

An idempotency key must be:

- **Unique**: Globally unique across all operations
- **Stable**: The same operation must always use the same key
- **Opaque**: The key format should not convey meaning
- **Time-bounded**: Keys should expire after a reasonable period

## Implementation

### Key Format

Idempotency keys should be UUIDs or similarly unique identifiers:

```
Example: 550e8400-e29b-41d4-a716-446655440000
```

### HTTP Header

Clients include the idempotency key in the request header:

```
X-Idempotency-Key: 550e8400-e29b-41d4-a716-446655440000
```

### Server-Side Processing

The server processes idempotency keys as follows:

1. **Extract Key**: Read the `X-Idempotency-Key` header
2. **Validate Key**: Ensure the key is present and properly formatted
3. **Check Cache**: Look for existing results with this key
4. **Return or Execute**: Return cached result if found, otherwise execute operation
5. **Cache Result**: Store the operation result with the key

## Usage in GraphQL

### Mutations Requiring Idempotency

All mutation operations in the GraphQL schema require idempotency keys:

```graphql
mutation {
  forceCompleteTransaction(id: "550e8400-e29b-41d4-a716-446655440000") {
    id
    status
  }
}
```

**HTTP Headers:**
```
X-Idempotency-Key: 6ba7b810-9dad-11d1-80b4-00c04fd430c8
```

### Queries (Idempotent by Nature)

Query operations do not require idempotency keys as they are inherently idempotent:

```graphql
query {
  transaction(id: "550e8400-e29b-41d4-a716-446655440000") {
    id
    status
  }
}
```

### Subscriptions (Long-lived)

Subscriptions do not require idempotency keys as they establish long-lived connections:

```graphql
subscription {
  transactionStatusChanged(transactionId: "550e8400-e29b-41d4-a716-446655440000") {
    transactionId
    status
  }
}
```

## Key Generation Strategies

### Strategy 1: Client-Generated UUID

Clients generate a UUID for each unique operation:

```javascript
const idempotencyKey = crypto.randomUUID();
```

**Pros:**
- Simple to implement
- Guarantees uniqueness
- No server coordination needed

**Cons:**
- Requires client to track keys
- Potential for key reuse if not careful

### Strategy 2: Operation-Specific Hash

Generate a hash based on operation parameters:

```javascript
const idempotencyKey = hash(
  operationName + 
  JSON.stringify(parameters) + 
  userId
);
```

**Pros:**
- Same operation always gets same key
- Automatic deduplication

**Cons:**
- Requires deterministic serialization
- May not work for operations with non-deterministic inputs

### Strategy 3: Server-Generated Key

Server generates keys and returns them to clients:

```javascript
// First request
POST /api/transactions
X-Idempotency-Key: auto
Response:
X-Idempotency-Key: 550e8400-e29b-41d4-a716-446655440000
```

**Pros:**
- Server controls key format
- Can embed metadata in key

**Cons:**
- Requires two round trips for first request
- More complex server logic

## Cache Management

### Cache Storage

Idempotency results are cached in a fast, distributed cache (e.g., Redis):

```
Key: idempotency:<idempotency-key>
Value: {
  "status": "completed",
  "result": { ... },
  "timestamp": "2024-01-01T00:00:00Z",
  "ttl": 86400
}
TTL: 24 hours
```

### Cache Expiration

Idempotency cache entries should expire after a reasonable period:

- **Default TTL**: 24 hours
- **Short-lived operations**: 1 hour
- **Long-running operations**: 7 days

### Cache Invalidation

Cache entries can be invalidated based on:

- Time-based expiration (TTL)
- Manual invalidation by admin
- Resource state changes (if applicable)

## Error Handling

### Missing Idempotency Key

```json
{
  "errors": [
    {
      "message": "X-Idempotency-Key header is required for mutations",
      "extensions": {
        "code": "IDEMPOTENCY_KEY_REQUIRED"
      }
    }
  ]
}
```

### Invalid Idempotency Key Format

```json
{
  "errors": [
    {
      "message": "Invalid idempotency key format",
      "extensions": {
        "code": "IDEMPOTENCY_KEY_INVALID"
      }
    }
  ]
}
```

### Key Conflict (Different Parameters)

```json
{
  "errors": [
    {
      "message": "Idempotency key already used with different parameters",
      "extensions": {
        "code": "IDEMPOTENCY_KEY_CONFLICT"
      }
    }
  ]
}
```

## Security Considerations

### Key Collision Prevention

- Use cryptographically secure random key generation
- Use sufficient key length (UUID v4: 122 random bits)
- Validate key format before processing

### Information Leakage

- Don't include sensitive data in idempotency keys
- Don't expose cache contents in error messages
- Log key access for audit purposes

### Rate Limiting

- Apply rate limiting based on idempotency keys
- Prevent key enumeration attacks
- Monitor for abuse patterns

## Best Practices

### For Clients

1. **Always Generate New Keys**: Generate a new key for each unique operation
2. **Retry with Same Key**: Always retry with the same idempotency key
3. **Handle Conflicts**: Gracefully handle key conflict errors
4. **Track Keys**: Maintain a mapping of operations to keys for debugging
5. **Set Timeouts**: Implement appropriate timeouts for idempotency checks

### For Servers

1. **Validate Keys**: Always validate key format before processing
2. **Set Appropriate TTL**: Configure cache TTL based on operation characteristics
3. **Monitor Cache**: Monitor cache hit rates and memory usage
4. **Log Access**: Log idempotency key access for audit trails
5. **Handle Edge Cases**: Handle cache failures gracefully

## Testing

### Unit Tests

Test idempotency key validation:

```rust
#[test]
fn test_validate_idempotency_key() {
    let valid_key = "550e8400-e29b-41d4-a716-446655440000";
    assert!(validate_idempotency_key(valid_key).is_ok());
    
    let invalid_key = "not-a-uuid";
    assert!(validate_idempotency_key(invalid_key).is_err());
}
```

### Integration Tests

Test end-to-end idempotency:

```rust
#[tokio::test]
async fn test_idempotent_mutation() {
    let key = Uuid::new_v4().to_string();
    
    // First request
    let result1 = execute_mutation_with_key(&key).await;
    
    // Second request with same key
    let result2 = execute_mutation_with_key(&key).await;
    
    // Results should be identical
    assert_eq!(result1, result2);
}
```

## Monitoring

### Metrics to Track

- Idempotency cache hit rate
- Idempotency cache miss rate
- Average cache lookup latency
- Number of key conflicts
- Cache memory usage

### Alerts

- Low cache hit rate (< 80%)
- High key conflict rate (> 1%)
- Cache memory exhaustion
- Slow cache lookups (> 10ms)

## References

- [RFC 9110: HTTP Semantics](https://httpwg.org/specs/rfc9110/)
- [Stripe Idempotency Documentation](https://stripe.com/docs/api/idempotency)
- [AWS Idempotency Best Practices](https://docs.aws.amazon.com/apigateway/latest/developerguide/api-gateway-api-usage-patterns-idempotency.html)
