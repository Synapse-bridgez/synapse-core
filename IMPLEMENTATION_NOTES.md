# Redis Idempotency Implementation - Issue #11

## Summary
Implemented Redis-based idempotency protection for webhook endpoints to prevent duplicate transaction processing when webhooks are delivered multiple times due to network retries.

## Changes Made

### 1. Dependencies Added
- `redis = { version = "0.24", features = ["tokio-comp", "connection-manager"] }` in Cargo.toml

### 2. Configuration Updates
- Added `redis_url: String` field to `Config` struct in `src/config.rs`
- Added `REDIS_URL` environment variable requirement
- Updated `.env.example` with Redis configuration

### 3. New Files Created

#### `src/middleware/mod.rs`
- Module declaration for middleware

#### `src/middleware/idempotency.rs`
- `IdempotencyService`: Manages Redis connections and idempotency state
- `idempotency_middleware`: Axum middleware for request interception
- `IdempotencyStatus` enum: Tracks request state (New, Processing, Completed)

Key features:
- Distributed lock using Redis with 5-minute TTL for processing state
- Response caching with 24-hour TTL for completed requests
- Automatic lock release on errors
- Fail-open behavior if Redis is unavailable

#### `src/handlers/webhook.rs`
- Basic webhook handler structure
- Accepts `WebhookPayload` with `id` and `anchor_transaction_id`
- Returns `WebhookResponse` with success status

### 4. Infrastructure Updates

#### `docker-compose.yml`
- Added Redis service (redis:7-alpine)
- Configured health checks for Redis
- Added Redis volume for data persistence
- Updated app service to depend on Redis
- Added `REDIS_URL` environment variable

### 5. Documentation

#### `docs/idempotency.md`
Comprehensive documentation covering:
- How idempotency works (request flow, TTL strategy)
- Configuration instructions
- Usage examples with curl commands
- Architecture overview
- Testing procedures
- Error handling strategies
- Security considerations
- Future enhancement ideas

#### Updated `README.md`
- Added Redis to prerequisites
- Updated environment variables section
- Added docker-compose instructions
- Updated webhook endpoint documentation
- Added reference to idempotency documentation

### 6. Integration Updates

#### `src/main.rs`
- Added `middleware` module import
- Imported `IdempotencyService`
- Initialized Redis idempotency service
- Created webhook routes with idempotency middleware layer
- Applied middleware only to webhook endpoints (not health check)

#### `src/handlers/mod.rs`
- Added `pub mod webhook` declaration

### 7. Testing

#### `tests/idempotency_test.rs`
- Created test file structure with placeholders
- Tests marked as `#[ignore]` since they require Redis
- Template for future integration tests

## How It Works

### Request Flow

1. **New Request**
   - Middleware checks Redis for `idempotency:{key}`
   - Key doesn't exist → Set to "PROCESSING" (5min TTL)
   - Process request normally
   - On success → Cache response (24h TTL)
   - On failure → Delete key to allow retry

2. **Duplicate During Processing**
   - Key exists with value "PROCESSING"
   - Return `429 Too Many Requests`
   - Client should retry after delay

3. **Duplicate After Completion**
   - Key exists with cached response
   - Return cached response with `cached: true`
   - No duplicate processing occurs

### Key Design Decisions

1. **Header-based Key**: Uses `X-Idempotency-Key` header for flexibility
2. **Two-stage TTL**: Short TTL for locks (5min), long TTL for cache (24h)
3. **Fail Open**: Redis failures don't block requests (logged for monitoring)
4. **Minimal Caching**: Currently caches success flag, can be extended to full response
5. **Middleware Pattern**: Clean separation of concerns, easy to apply to specific routes

## Testing Instructions

### Manual Testing

1. Start services:
```bash
docker-compose up -d
```

2. Test new request:
```bash
curl -X POST http://localhost:3000/webhook \
  -H "Content-Type: application/json" \
  -H "X-Idempotency-Key: test-123" \
  -d '{"id": "w1", "anchor_transaction_id": "test-123"}'
```

3. Test duplicate (immediate):
```bash
# Should return 429 if still processing
curl -X POST http://localhost:3000/webhook \
  -H "Content-Type: application/json" \
  -H "X-Idempotency-Key: test-123" \
  -d '{"id": "w1", "anchor_transaction_id": "test-123"}'
```

4. Test duplicate (after completion):
```bash
# Wait a few seconds, should return cached response
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

## Security & Reliability

### Security
- No sensitive data stored in Redis
- Keys automatically expire (no memory leaks)
- Input validation on idempotency keys
- Fail-open prevents DoS via Redis

### Reliability
- Automatic lock expiration prevents stuck locks
- Redis connection pooling for performance
- Graceful degradation on Redis failure
- Comprehensive error logging

## Future Enhancements

1. **Full Response Caching**: Store complete response body for exact replay
2. **Distributed Locking**: Implement Redlock for multi-instance deployments
3. **Metrics**: Add Prometheus metrics for duplicate rates and cache hits
4. **Configurable TTLs**: Make TTL values environment-configurable
5. **Integration Tests**: Add comprehensive tests with test Redis instance
6. **Body-based Keys**: Support extracting idempotency key from request body

## Branch Information
- Branch: `feature/issue-11-redis-idempotency`
- Target: `develop` branch
- Issue: #11 - Webhook Idempotency with Redis

## Files Modified
- `Cargo.toml`
- `src/config.rs`
- `src/main.rs`
- `src/handlers/mod.rs`
- `docker-compose.yml`
- `README.md`
- `.env.example`

## Files Created
- `src/middleware/mod.rs`
- `src/middleware/idempotency.rs`
- `src/handlers/webhook.rs`
- `docs/idempotency.md`
- `tests/idempotency_test.rs`
- `IMPLEMENTATION_NOTES.md` (this file)
