# Webhook Replay Admin Interface (Issue #98)

## Overview

The webhook replay admin interface enables operators to replay historical webhook payloads for debugging and recovery from processing failures. This is essential when processing logic changes or bugs are fixed, allowing operators to reprocess failed webhooks without waiting for the anchor to resend them.

## Features

### 1. Store Original Webhook Payloads
- All webhook payloads are automatically stored in the `transactions` table
- Original payload data is preserved in audit logs via the audit logging system
- Metadata and callback information are retained for replay

### 2. List Failed Webhook Attempts
- Query failed webhooks with filtering options:
  - By asset code
  - By date range (from/to)
  - Pagination support (limit/offset)
- View retry counts and error messages from DLQ
- See transaction details including amounts and stellar accounts

### 3. Replay Individual Webhooks
- Replay a single webhook by transaction ID
- Dry-run mode to test without committing changes
- Automatic audit logging of replay attempts
- Respects idempotency constraints

### 4. Batch Replay
- Replay multiple webhooks in a single request
- Dry-run mode for batch operations
- Detailed results for each transaction
- Success/failure tracking

### 5. Replay History Tracking
- All replay attempts are tracked in `webhook_replay_history` table
- Records:
  - Transaction ID
  - Who initiated the replay
  - Dry-run vs actual replay
  - Success/failure status
  - Error messages
  - Timestamp

## API Endpoints

All endpoints require admin authentication via the `admin_auth` middleware.

### List Failed Webhooks

```
GET /admin/webhooks/failed
```

**Query Parameters:**
- `limit` (optional, default: 50, max: 100): Number of results to return
- `offset` (optional, default: 0): Pagination offset
- `asset_code` (optional): Filter by asset code (e.g., "USDC")
- `from_date` (optional): Filter by start date (ISO 8601 format)
- `to_date` (optional): Filter by end date (ISO 8601 format)

**Response:**
```json
{
  "total": 42,
  "webhooks": [
    {
      "transaction_id": "550e8400-e29b-41d4-a716-446655440000",
      "stellar_account": "GABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890ABCDEFGHIJKLMNOP",
      "amount": "100.50",
      "asset_code": "USDC",
      "anchor_transaction_id": "anchor-tx-12345",
      "status": "failed",
      "created_at": "2026-02-23T10:30:00Z",
      "last_error": "Network timeout during processing",
      "retry_count": 3
    }
  ]
}
```

### Replay Single Webhook

```
POST /admin/webhooks/replay/:transaction_id
```

**Path Parameters:**
- `transaction_id`: UUID of the transaction to replay

**Request Body:**
```json
{
  "dry_run": false
}
```

**Response:**
```json
{
  "transaction_id": "550e8400-e29b-41d4-a716-446655440000",
  "success": true,
  "message": "Webhook replayed successfully",
  "dry_run": false,
  "replayed_at": "2026-02-23T11:00:00Z"
}
```

### Batch Replay Webhooks

```
POST /admin/webhooks/replay/batch
```

**Request Body:**
```json
{
  "transaction_ids": [
    "550e8400-e29b-41d4-a716-446655440000",
    "660e8400-e29b-41d4-a716-446655440001",
    "770e8400-e29b-41d4-a716-446655440002"
  ],
  "dry_run": false
}
```

**Response:**
```json
{
  "total": 3,
  "successful": 2,
  "failed": 1,
  "results": [
    {
      "transaction_id": "550e8400-e29b-41d4-a716-446655440000",
      "success": true,
      "message": "Webhook replayed successfully",
      "dry_run": false,
      "replayed_at": "2026-02-23T11:00:00Z"
    },
    {
      "transaction_id": "660e8400-e29b-41d4-a716-446655440001",
      "success": true,
      "message": "Webhook replayed successfully",
      "dry_run": false,
      "replayed_at": "2026-02-23T11:00:01Z"
    },
    {
      "transaction_id": "770e8400-e29b-41d4-a716-446655440002",
      "success": false,
      "message": "Cannot replay completed transaction without dry-run mode",
      "dry_run": false,
      "replayed_at": null
    }
  ]
}
```

## Usage Examples

### Example 1: List Failed Webhooks

```bash
curl -X GET "http://localhost:3000/admin/webhooks/failed?limit=10&asset_code=USDC" \
  -H "Authorization: Bearer <admin-token>"
```

### Example 2: Dry-Run Replay (Test Mode)

```bash
curl -X POST "http://localhost:3000/admin/webhooks/replay/550e8400-e29b-41d4-a716-446655440000" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer <admin-token>" \
  -d '{
    "dry_run": true
  }'
```

### Example 3: Actual Replay

```bash
curl -X POST "http://localhost:3000/admin/webhooks/replay/550e8400-e29b-41d4-a716-446655440000" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer <admin-token>" \
  -d '{
    "dry_run": false
  }'
```

### Example 4: Batch Replay with Dry-Run

```bash
curl -X POST "http://localhost:3000/admin/webhooks/replay/batch" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer <admin-token>" \
  -d '{
    "transaction_ids": [
      "550e8400-e29b-41d4-a716-446655440000",
      "660e8400-e29b-41d4-a716-446655440001"
    ],
    "dry_run": true
  }'
```

## Implementation Details

### Files Created/Modified

1. **src/handlers/admin/webhook_replay.rs** (NEW)
   - Core replay logic
   - API endpoint handlers
   - Payload retrieval from audit logs
   - Replay tracking

2. **src/handlers/admin/mod.rs** (MODIFIED)
   - Added webhook_replay module
   - Created webhook_replay_routes() function

3. **src/main.rs** (MODIFIED)
   - Registered webhook replay routes under `/admin`

4. **src/db/queries.rs** (MODIFIED)
   - Added `get_audit_logs()` function for retrieving audit history

5. **migrations/20260223000000_webhook_replay_tracking.sql** (NEW)
   - Created `webhook_replay_history` table
   - Added indexes for efficient queries

### Database Schema

#### webhook_replay_history Table

```sql
CREATE TABLE webhook_replay_history (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    transaction_id UUID NOT NULL REFERENCES transactions(id),
    replayed_by VARCHAR(255) NOT NULL DEFAULT 'admin',
    dry_run BOOLEAN NOT NULL DEFAULT false,
    success BOOLEAN NOT NULL,
    error_message TEXT,
    replayed_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
```

**Indexes:**
- `idx_webhook_replay_history_transaction_id`: Fast lookups by transaction
- `idx_webhook_replay_history_replayed_at`: Time-based queries
- `idx_webhook_replay_history_success`: Filter by success status

## Security Considerations

### Authentication
- All replay endpoints require admin authentication
- Uses the existing `admin_auth` middleware
- Unauthorized requests return 401 Unauthorized

### Idempotency
- Replays respect existing idempotency keys
- Completed transactions cannot be replayed without dry-run mode
- Prevents accidental duplicate processing

### Audit Trail
- All replay attempts are logged in `audit_logs` table
- Replay history tracked in `webhook_replay_history` table
- Includes actor information (who initiated the replay)
- Timestamps for forensic analysis

## Constraints and Limitations

### Idempotency Constraint
- Replays must respect idempotency keys
- Completed transactions require dry-run mode
- Prevents duplicate processing of successful transactions

### Status Validation
- Only failed or pending transactions can be replayed
- Completed transactions are protected unless in dry-run mode
- Status transitions are validated before replay

### Rate Limiting
- Consider implementing rate limits for batch replays
- Large batch operations may impact database performance
- Recommend processing in smaller batches (e.g., 50-100 at a time)

## Monitoring and Observability

### Logging
All replay operations are logged with:
- Transaction ID
- Dry-run status
- Success/failure
- Error messages
- Timestamp

Example log output:
```
INFO Replaying webhook for transaction 550e8400-e29b-41d4-a716-446655440000 (dry_run: false)
INFO Transaction 550e8400-e29b-41d4-a716-446655440000 status updated to pending for reprocessing
```

### Metrics
Consider adding metrics for:
- Total replay attempts
- Success/failure rates
- Dry-run vs actual replays
- Average replay time
- Batch replay sizes

## Testing

### Unit Tests
The implementation includes unit tests for:
- Default limit values
- Serialization of response types
- Batch replay response structure

### Integration Testing
To test the webhook replay functionality:

1. Create a failed transaction:
```bash
# Insert a test transaction with failed status
psql $DATABASE_URL -c "
INSERT INTO transactions (id, stellar_account, amount, asset_code, status, created_at, updated_at)
VALUES (gen_random_uuid(), 'GABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890ABCDEFGHIJKLMNOP', 100.50, 'USDC', 'failed', NOW(), NOW());
"
```

2. List failed webhooks:
```bash
curl -X GET "http://localhost:3000/admin/webhooks/failed" \
  -H "Authorization: Bearer <admin-token>"
```

3. Test dry-run replay:
```bash
curl -X POST "http://localhost:3000/admin/webhooks/replay/<transaction-id>" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer <admin-token>" \
  -d '{"dry_run": true}'
```

4. Verify replay history:
```bash
psql $DATABASE_URL -c "SELECT * FROM webhook_replay_history ORDER BY replayed_at DESC LIMIT 10;"
```

## Future Enhancements

1. **Scheduled Replays**: Ability to schedule replays for specific times
2. **Replay Filters**: More advanced filtering options (by error type, retry count, etc.)
3. **Replay Policies**: Configurable policies for automatic replay of certain failure types
4. **Webhook Validation**: Pre-replay validation of webhook payloads
5. **Replay Metrics Dashboard**: Visual dashboard for monitoring replay operations
6. **Bulk Operations**: Support for replaying all failed webhooks matching criteria
7. **Replay Throttling**: Built-in rate limiting for large batch operations

## Troubleshooting

### Common Issues

**Issue: "Transaction not found"**
- Verify the transaction ID exists in the database
- Check that the transaction hasn't been deleted

**Issue: "Cannot replay completed transaction"**
- Use dry-run mode to test completed transactions
- Only failed/pending transactions can be replayed without dry-run

**Issue: "Database error during replay"**
- Check database connectivity
- Verify transaction table partitions exist
- Review database logs for detailed errors

**Issue: "Unauthorized"**
- Ensure admin authentication token is valid
- Verify admin_auth middleware is configured correctly

## Related Documentation

- [Audit Logging](./audit_logging.md) - How webhook payloads are stored
- [Idempotency](./idempotency.md) - Idempotency constraints and behavior
- [Webhook Handler](./webhook-handler.md) - Original webhook processing logic
- [DLQ](./dlq.md) - Dead Letter Queue for failed transactions
