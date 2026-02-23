# Dead Letter Queue (DLQ) Implementation

## Overview

The DLQ system handles transactions that fail processing after multiple retry attempts. It provides:

1. **Exponential backoff** for transient errors
2. **Automatic DLQ movement** after max retries
3. **Manual requeue** capability via API

## Configuration

- `MAX_RETRIES`: 3 attempts
- `BASE_DELAY_MS`: 100ms (exponential: 100ms, 200ms, 400ms)

## Database Schema

### `transaction_dlq` Table

```sql
CREATE TABLE transaction_dlq (
    id UUID PRIMARY KEY,
    transaction_id UUID NOT NULL,
    stellar_account VARCHAR(56) NOT NULL,
    amount NUMERIC NOT NULL,
    asset_code VARCHAR(12) NOT NULL,
    anchor_transaction_id VARCHAR(255),
    error_reason TEXT NOT NULL,
    stack_trace TEXT,
    retry_count INTEGER NOT NULL DEFAULT 0,
    original_created_at TIMESTAMPTZ NOT NULL,
    moved_to_dlq_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_retry_at TIMESTAMPTZ
);
```

## API Endpoints

### List DLQ Entries

```bash
GET /dlq
```

Response:
```json
{
  "dlq_entries": [...],
  "count": 5
}
```

### Requeue DLQ Entry

```bash
POST /dlq/{id}/requeue
```

Response:
```json
{
  "message": "DLQ entry requeued successfully",
  "dlq_id": "uuid"
}
```

## Usage

### Processing with Retry Logic

```rust
use synapse_core::services::TransactionProcessor;

let processor = TransactionProcessor::new(pool);
processor.process_transaction(tx_id).await?;
```

The processor automatically:
1. Attempts processing
2. Retries on transient errors (pool timeout, IO errors)
3. Moves to DLQ after max retries
4. Updates transaction status to 'dlq'

### Requeuing from DLQ

```rust
processor.requeue_dlq(dlq_id).await?;
```

This:
1. Resets transaction status to 'pending'
2. Removes entry from DLQ
3. Allows reprocessing

## Error Classification

**Transient Errors** (retried):
- Database pool timeouts
- IO errors

**Permanent Errors** (immediate DLQ):
- Validation errors
- Logic errors
- All other errors

## Monitoring

Check DLQ entries regularly:

```bash
curl http://localhost:3000/dlq
```

Investigate error_reason and stack_trace for debugging.
