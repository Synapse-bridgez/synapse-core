# Idempotency Implementation

## Overview

This implementation provides webhook idempotency using Redis to prevent duplicate transaction processing when webhooks are delivered multiple times due to network retries.

## How It Works

### 1. Idempotency Key
- Webhooks must include an `X-Idempotency-Key` header (typically the `anchor_transaction_id`)
- This key uniquely identifies each webhook request

### 2. Request Flow

#### First Request (New)
1. Client sends webhook with `X-Idempotency-Key: transaction-123`
2. Middleware checks Redis for key `idempotency:transaction-123`
3. Key doesn't exist → Set key to "PROCESSING" with 5-minute TTL
4. Process the webhook normally
5. On success (2xx response) → Store response in Redis with 24-hour TTL
6. On failure → Delete the key to allow retry

#### Duplicate Request (Processing)
1. Client sends same webhook while first is still processing
2. Middleware finds key with value "PROCESSING"
3. Return `429 Too Many Requests` with retry-after header
4. Client should wait and retry

#### Duplicate Request (Completed)
1. Client sends same webhook after successful processing
2. Middleware finds key with cached response
3. Return cached response (200 OK) with `cached: true` flag
4. No duplicate processing occurs

### 3. TTL Strategy
- **Processing Lock**: 5 minutes (prevents stuck locks from failed requests)
- **Completed Response**: 24 hours (prevents duplicate processing within reasonable window)

## Configuration

### Environment Variables
```bash
REDIS_URL=redis://localhost:6379
```

### Docker Compose
Redis is automatically configured in `docker-compose.yml`:
```yaml
redis:
  image: redis:7-alpine
  ports:
    - "6379:6379"
```

## Usage

### Making Idempotent Webhook Requests

```bash
curl -X POST http://localhost:3000/webhook \
  -H "Content-Type: application/json" \
  -H "X-Idempotency-Key: anchor-tx-12345" \
  -d '{
    "id": "webhook-001",
    "anchor_transaction_id": "anchor-tx-12345"
  }'
```

### Response Scenarios

#### Success (First Request)
```json
{
  "success": true,
  "message": "Webhook webhook-001 processed successfully"
}
```
Status: `200 OK`

#### Processing (Duplicate During Processing)
```json
{
  "error": "Request is currently being processed",
  "retry_after": 5
}
```
Status: `429 Too Many Requests`

#### Cached (Duplicate After Completion)
```json
{
  "cached": true,
  "message": "Request already processed"
}
```
Status: `200 OK`

## Architecture

### Components

1. **IdempotencyService** (`src/middleware/idempotency.rs`)
   - Manages Redis connections
   - Provides methods for checking and storing idempotency state
   - Handles lock acquisition and release

2. **Idempotency Middleware** (`src/middleware/idempotency.rs`)
   - Axum middleware that wraps webhook handlers
   - Extracts idempotency key from headers
   - Coordinates request flow based on idempotency status

3. **Webhook Handler** (`src/handlers/webhook.rs`)
   - Business logic for processing webhooks
   - Protected by idempotency middleware

### Redis Key Structure
```
idempotency:{anchor_transaction_id} → "PROCESSING" | CachedResponse
```

## Testing

### Manual Testing

1. Start services:
```bash
docker-compose up -d
```

2. Send first request:
```bash
curl -X POST http://localhost:3000/webhook \
  -H "Content-Type: application/json" \
  -H "X-Idempotency-Key: test-123" \
  -d '{"id": "w1", "anchor_transaction_id": "test-123"}'
```

3. Send duplicate immediately (should get 429):
```bash
curl -X POST http://localhost:3000/webhook \
  -H "Content-Type: application/json" \
  -H "X-Idempotency-Key: test-123" \
  -d '{"id": "w1", "anchor_transaction_id": "test-123"}'
```

4. Wait a few seconds and send again (should get cached response):
```bash
curl -X POST http://localhost:3000/webhook \
  -H "Content-Type: application/json" \
  -H "X-Idempotency-Key: test-123" \
  -d '{"id": "w1", "anchor_transaction_id": "test-123"}'
```

### Verify Redis State
```bash
docker exec -it synapse-redis redis-cli
> KEYS idempotency:*
> GET idempotency:test-123
> TTL idempotency:test-123
```

## Error Handling

### Redis Connection Failure
- Middleware fails open (allows request to proceed)
- Logs error for monitoring
- Prevents Redis outage from blocking all webhooks

### Processing Timeout
- Processing lock expires after 5 minutes
- Allows retry if original request failed/hung
- Prevents permanent lock from crashed requests

## Security Considerations

1. **Key Validation**: Idempotency keys are validated for proper format
2. **TTL Limits**: Keys automatically expire to prevent Redis memory exhaustion
3. **Fail Open**: Redis failures don't block legitimate requests
4. **No Sensitive Data**: Only status codes and success flags stored in Redis

## Future Enhancements

1. **Response Body Caching**: Store full response body for exact replay
2. **Distributed Locking**: Use Redlock algorithm for multi-instance deployments
3. **Metrics**: Track duplicate request rates and cache hit ratios
4. **Configurable TTLs**: Make TTL values configurable per environment
