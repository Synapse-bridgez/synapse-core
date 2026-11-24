# Idempotency Keys in GraphQL

## Overview

Idempotency keys ensure that GraphQL mutations can be safely retried without causing duplicate side effects. This is critical for distributed systems where network failures may cause clients to retry requests.

## Implementation

### Idempotency Key Header

All GraphQL mutations must include an `X-Idempotency-Key` header:

```graphql
POST /graphql HTTP/1.1
X-Idempotency-Key: unique-key-12345
Content-Type: application/json

{
  "query": "mutation { forceCompleteTransaction(id: \"...\") { id status } }"
}
```

### Key Requirements

- **Uniqueness**: Each idempotency key must be unique per mutation operation
- **Stability**: The same key must be used for retries of the same operation
- **Format**: String value, typically UUID or transaction ID
- **Lifetime**: Keys are cached for 24 hours by default

### Recommended Key Formats

1. **Transaction-based**: Use the transaction ID
   ```
   X-Idempotency-Key: 550e8400-e29b-41d4-a716-446655440000
   ```

2. **Request-based**: Use a UUID generated per request
   ```
   X-Idempotency-Key: req-2024-05-26-abc123def456
   ```

3. **Anchor-based**: Use anchor transaction ID
   ```
   X-Idempotency-Key: anchor-tx-12345
   ```

## Behavior

### Successful Response (2xx)

When a mutation succeeds:
1. Response is cached with the idempotency key
2. Subsequent requests with the same key return the cached response
3. No duplicate side effects occur

### Retry Handling

When retrying a mutation:
1. Client sends the same `X-Idempotency-Key`
2. Server checks Redis cache for existing response
3. If found, returns cached response immediately
4. If not found, executes mutation and caches result

### Concurrent Requests

If two requests arrive simultaneously with the same key:
1. First request acquires lock and executes
2. Second request waits (returns 429 Too Many Requests if timeout)
3. Both receive the same response

## Error Handling

### Missing Idempotency Key

```
HTTP/1.1 400 Bad Request
{
  "errors": [
    {
      "message": "X-Idempotency-Key header is required for mutations"
    }
  ]
}
```

### Concurrent Request Conflict

```
HTTP/1.1 429 Too Many Requests
{
  "errors": [
    {
      "message": "Concurrent request with same idempotency key in progress"
    }
  ]
}
```

### Cache Retrieval Error

```
HTTP/1.1 500 Internal Server Error
{
  "errors": [
    {
      "message": "Idempotency cache unavailable"
    }
  ]
}
```

## Configuration

### Environment Variables

```bash
# Redis connection for idempotency cache
REDIS_URL=redis://localhost:6379

# Idempotency key TTL (seconds)
IDEMPOTENCY_KEY_TTL=86400  # 24 hours

# Lock timeout for concurrent requests (milliseconds)
IDEMPOTENCY_LOCK_TIMEOUT=5000  # 5 seconds
```

### Defaults

- **Cache TTL**: 24 hours
- **Lock Timeout**: 5 seconds
- **Max Concurrent**: Unlimited (per key)

## Examples

### Mutation with Idempotency

```graphql
mutation CompleteTransaction($id: UUID!) {
  forceCompleteTransaction(id: $id) {
    id
    status
    updatedAt
  }
}
```

**Request:**
```bash
curl -X POST http://localhost:3000/graphql \
  -H "Content-Type: application/json" \
  -H "X-Idempotency-Key: txn-550e8400-e29b-41d4-a716-446655440000" \
  -d '{
    "query": "mutation { forceCompleteTransaction(id: \"550e8400-e29b-41d4-a716-446655440000\") { id status } }"
  }'
```

**Response (First Call):**
```json
{
  "data": {
    "forceCompleteTransaction": {
      "id": "550e8400-e29b-41d4-a716-446655440000",
      "status": "completed"
    }
  }
}
```

**Response (Retry with Same Key):**
```json
{
  "data": {
    "forceCompleteTransaction": {
      "id": "550e8400-e29b-41d4-a716-446655440000",
      "status": "completed"
    }
  }
}
```

## Best Practices

1. **Always use idempotency keys for mutations**
   - Prevents accidental duplicates
   - Enables safe retries

2. **Use stable, deterministic keys**
   - Avoid random UUIDs per request
   - Use transaction IDs or request IDs

3. **Implement client-side retry logic**
   - Retry on network errors
   - Use exponential backoff
   - Respect 429 responses

4. **Monitor idempotency metrics**
   - Track cache hits vs misses
   - Monitor lock contention
   - Alert on high error rates

## Monitoring

### Metrics

The following metrics are tracked:

- `idempotency_cache_hits_total`: Number of cached responses returned
- `idempotency_cache_misses_total`: Number of cache misses
- `idempotency_lock_acquired_total`: Number of locks successfully acquired
- `idempotency_lock_contention_total`: Number of concurrent request conflicts
- `idempotency_errors_total`: Number of idempotency errors
- `idempotency_fallback_count_total`: Number of fallback to database

### Example Prometheus Query

```promql
# Cache hit rate
rate(idempotency_cache_hits_total[5m]) / 
(rate(idempotency_cache_hits_total[5m]) + rate(idempotency_cache_misses_total[5m]))

# Lock contention rate
rate(idempotency_lock_contention_total[5m])
```

## Troubleshooting

### High Cache Miss Rate

**Symptom**: Most requests result in cache misses

**Causes**:
- Clients not reusing idempotency keys
- Keys expiring too quickly
- Redis connection issues

**Solution**:
- Verify clients are using stable keys
- Increase `IDEMPOTENCY_KEY_TTL` if needed
- Check Redis connectivity

### High Lock Contention

**Symptom**: Frequent 429 responses

**Causes**:
- Multiple clients retrying simultaneously
- Lock timeout too short
- High request volume

**Solution**:
- Increase `IDEMPOTENCY_LOCK_TIMEOUT`
- Implement exponential backoff on client
- Scale Redis cluster

### Cache Unavailable Errors

**Symptom**: 500 errors with "cache unavailable"

**Causes**:
- Redis connection failure
- Network partition
- Redis memory exhausted

**Solution**:
- Check Redis health
- Verify network connectivity
- Monitor Redis memory usage
- Enable fallback to database (if configured)

## Security Considerations

1. **Key Validation**
   - Keys are validated for format and length
   - Malformed keys are rejected

2. **Cache Isolation**
   - Each tenant's cache is isolated
   - Keys are namespaced by tenant ID

3. **Response Caching**
   - Only successful responses (2xx) are cached
   - Error responses are not cached
   - Sensitive data is not logged

## References

- [Idempotency Documentation](./idempotency.md)
- [GraphQL Best Practices](https://graphql.org/learn/best-practices/)
- [HTTP Idempotency RFC 9110](https://tools.ietf.org/html/rfc9110#section-9.2.2)

## `first` and `last` are mutually exclusive

A cursor pagination request may set `first` (page forward, usually with `after`) or `last` (page backward, usually with `before`), never both. Sending both is rejected before any query runs with a GraphQL validation error, `extensions.code = "VALIDATION_ERROR"`, whose message names both `first` and `last`. The check lives in `PaginationArgs::validate` (`src/graphql/pagination.rs`); `CursorPagination::new` enforces the same rule for code that builds cursor parameters directly.
