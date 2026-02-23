# Pull Request: Dead Letter Queue (DLQ) Implementation

## Summary

Implements a Dead Letter Queue system for handling failed transaction processing with exponential backoff retry logic and manual requeue capability.

## Changes

### 1. Database Migration
- **File**: `migrations/20260220143500_transaction_dlq.sql`
- Created `transaction_dlq` table with fields:
  - Transaction details (id, stellar_account, amount, asset_code)
  - Error tracking (error_reason, stack_trace, retry_count)
  - Timestamps (original_created_at, moved_to_dlq_at, last_retry_at)
- Added indexes for performance

### 2. Data Models
- **File**: `src/db/models.rs`
- Added `TransactionDlq` struct
- Implemented `bigdecimal_serde` module for BigDecimal serialization

### 3. Transaction Processor
- **File**: `src/services/transaction_processor.rs`
- Implemented `TransactionProcessor` with:
  - `process_transaction()`: Main processing with retry logic
  - `move_to_dlq()`: Moves failed transactions to DLQ
  - `requeue_dlq()`: Requeues DLQ entries for reprocessing
  - Exponential backoff: 100ms, 200ms, 400ms
  - Max retries: 3 attempts
  - Transient error detection (pool timeout, IO errors)

### 4. API Endpoints
- **File**: `src/handlers/dlq.rs`
- `GET /dlq`: List all DLQ entries (limit 100)
- `POST /dlq/:id/requeue`: Requeue a DLQ entry

### 5. Integration
- **Files**: `src/main.rs`, `src/lib.rs`, `src/handlers/mod.rs`, `src/services/mod.rs`
- Integrated DLQ routes into main router
- Exported TransactionProcessor service
- Fixed config.rs to remove unused ipnet dependency
- Fixed BigDecimal serde issues

### 6. Documentation
- **File**: `docs/dlq.md`
- Complete DLQ usage guide
- API documentation
- Error classification
- Monitoring guidelines

## Testing

Build successful:
```bash
cargo build
# Finished `dev` profile [unoptimized + debuginfo] target(s) in 43.07s
```

## API Usage Examples

### List DLQ entries:
```bash
curl http://localhost:3000/dlq
```

### Requeue a DLQ entry:
```bash
curl -X POST http://localhost:3000/dlq/{uuid}/requeue
```

## Error Handling

**Transient Errors** (retried with exponential backoff):
- Database pool timeouts
- IO errors

**Permanent Errors** (immediate DLQ):
- Validation errors
- Logic errors
- All other application errors

## Configuration

Constants in `src/services/transaction_processor.rs`:
- `MAX_RETRIES = 3`
- `BASE_DELAY_MS = 100`

## Database Schema

The DLQ table stores:
- Full transaction details for replay
- Error diagnostics (reason + stack trace)
- Retry history
- Timestamps for monitoring

## Next Steps

1. Add integration tests for retry logic
2. Add metrics/monitoring for DLQ size
3. Consider adding batch requeue endpoint
4. Add DLQ entry expiration policy

## Checklist

- [x] Create feature branch
- [x] Create transaction_dlq table migration
- [x] Implement TransactionProcessor with retry logic
- [x] Implement requeue_dlq function
- [x] Add API endpoints for DLQ management
- [x] Add documentation
- [x] Code compiles successfully
- [ ] Run integration tests
- [ ] Submit PR against develop branch
