# Webhook Replay Implementation - Issue #98

## Summary

This implementation provides a complete admin interface for replaying historical webhook payloads, enabling debugging and recovery from processing failures.

## Implementation Checklist

- [x] Create feature branch: `feature/issue-98-webhook-replay`
- [x] Implement `src/handlers/admin/webhook_replay.rs` with core replay logic
- [x] Add payload retrieval from audit logs
- [x] Implement list failed webhooks endpoint
- [x] Implement single webhook replay endpoint with dry-run support
- [x] Implement batch webhook replay endpoint
- [x] Add replay history tracking in database
- [x] Create migration for `webhook_replay_history` table
- [x] Update `src/handlers/admin/mod.rs` to export webhook_replay module
- [x] Register routes in `src/main.rs`
- [x] Add `get_audit_logs()` query function to `src/db/queries.rs`
- [x] Create comprehensive documentation in `docs/webhook-replay.md`
- [x] Add unit tests for core functionality
- [x] Create integration tests in `tests/webhook_replay_test.rs`

## Files Created

1. **src/handlers/admin/webhook_replay.rs** (NEW)
   - Core webhook replay functionality
   - API endpoint handlers
   - Payload retrieval and validation
   - Replay tracking

2. **src/handlers/admin/mod.rs** (NEW)
   - Admin module organization
   - Route registration for webhook replay

3. **migrations/20260223000000_webhook_replay_tracking.sql** (NEW)
   - Database schema for replay history tracking
   - Indexes for efficient queries

4. **docs/webhook-replay.md** (NEW)
   - Complete documentation
   - API reference
   - Usage examples
   - Security considerations

5. **tests/webhook_replay_test.rs** (NEW)
   - Integration tests for replay functionality

6. **WEBHOOK_REPLAY_IMPLEMENTATION.md** (NEW)
   - This implementation summary

## Files Modified

1. **src/main.rs**
   - Added webhook replay routes under `/admin`
   - Routes protected by admin authentication

2. **src/db/queries.rs**
   - Added `get_audit_logs()` function for retrieving audit history

## API Endpoints

### 1. List Failed Webhooks
```
GET /admin/webhooks/failed
```
Query parameters: `limit`, `offset`, `asset_code`, `from_date`, `to_date`

### 2. Replay Single Webhook
```
POST /admin/webhooks/replay/:transaction_id
```
Body: `{ "dry_run": boolean }`

### 3. Batch Replay Webhooks
```
POST /admin/webhooks/replay/batch
```
Body: `{ "transaction_ids": [uuid, ...], "dry_run": boolean }`

## Key Features

### Payload Storage
- Original webhook payloads stored in `transactions` table
- Audit logs preserve complete transaction history
- Metadata and callback information retained

### Dry-Run Mode
- Test replays without committing changes
- Validates payload and processing logic
- Safe for production testing

### Replay Tracking
- All replay attempts logged in `webhook_replay_history` table
- Tracks success/failure, error messages, timestamps
- Audit trail for compliance

### Idempotency Respect
- Completed transactions protected from accidental replay
- Idempotency keys respected during replay
- Status validation before processing

### Batch Operations
- Replay multiple webhooks in single request
- Individual result tracking per transaction
- Success/failure summary

## Database Schema

### webhook_replay_history Table
```sql
CREATE TABLE webhook_replay_history (
    id UUID PRIMARY KEY,
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
- `idx_webhook_replay_history_transaction_id`
- `idx_webhook_replay_history_replayed_at`
- `idx_webhook_replay_history_success`

## Security

### Authentication
- All endpoints require admin authentication
- Uses existing `admin_auth` middleware
- Unauthorized requests return 401

### Audit Trail
- All replays logged in `audit_logs` table
- Replay history in `webhook_replay_history` table
- Actor tracking (who initiated replay)
- Timestamp tracking for forensics

### Constraints
- Idempotency keys must be respected
- Completed transactions require dry-run mode
- Status transitions validated

## Testing

### Unit Tests
Located in `src/handlers/admin/webhook_replay.rs`:
- Default limit values
- Serialization tests
- Response structure validation

### Integration Tests
Located in `tests/webhook_replay_test.rs`:
- Replay tracking verification
- Failed webhook listing
- Status update validation

### Manual Testing

1. **List failed webhooks:**
```bash
curl -X GET "http://localhost:3000/admin/webhooks/failed?limit=10" \
  -H "Authorization: Bearer <admin-token>"
```

2. **Dry-run replay:**
```bash
curl -X POST "http://localhost:3000/admin/webhooks/replay/<tx-id>" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer <admin-token>" \
  -d '{"dry_run": true}'
```

3. **Actual replay:**
```bash
curl -X POST "http://localhost:3000/admin/webhooks/replay/<tx-id>" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer <admin-token>" \
  -d '{"dry_run": false}'
```

4. **Batch replay:**
```bash
curl -X POST "http://localhost:3000/admin/webhooks/replay/batch" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer <admin-token>" \
  -d '{
    "transaction_ids": ["<tx-id-1>", "<tx-id-2>"],
    "dry_run": false
  }'
```

## Dependencies

This implementation depends on:
- Issue #2: Webhook handler (for original payload structure)
- Issue #20: Audit logging (for payload storage and retrieval)

## Usage Workflow

1. **Identify Failed Webhooks**
   - Use `GET /admin/webhooks/failed` to list failed transactions
   - Filter by asset code, date range, or other criteria

2. **Test Replay (Dry-Run)**
   - Use dry-run mode to validate replay logic
   - Verify payload and processing without committing

3. **Execute Replay**
   - Replay individual webhooks or batch
   - Monitor results and error messages

4. **Verify Results**
   - Check transaction status updates
   - Review replay history in database
   - Verify audit logs

## Monitoring

### Logging
All replay operations logged with:
- Transaction ID
- Dry-run status
- Success/failure
- Error messages
- Timestamps

### Database Queries
Monitor replay history:
```sql
-- Recent replay attempts
SELECT * FROM webhook_replay_history 
ORDER BY replayed_at DESC 
LIMIT 20;

-- Success rate
SELECT 
    COUNT(*) as total,
    SUM(CASE WHEN success THEN 1 ELSE 0 END) as successful,
    SUM(CASE WHEN NOT success THEN 1 ELSE 0 END) as failed
FROM webhook_replay_history
WHERE replayed_at > NOW() - INTERVAL '24 hours';
```

## Future Enhancements

1. **Scheduled Replays**: Cron-based replay scheduling
2. **Advanced Filtering**: More query options for failed webhooks
3. **Replay Policies**: Automatic replay rules
4. **Metrics Dashboard**: Visual monitoring interface
5. **Bulk Operations**: Replay all matching criteria
6. **Rate Limiting**: Built-in throttling for large batches

## Deployment Notes

### Migration
Run migrations before deploying:
```bash
sqlx migrate run
```

### Configuration
No additional configuration required. Uses existing:
- Database connection pool
- Admin authentication
- Audit logging system

### Rollback
If needed, rollback migration:
```bash
sqlx migrate revert
```

## PR Submission

Submit PR against the `develop` branch with:
- All implementation files
- Documentation
- Tests
- Migration scripts

## Related Documentation

- [docs/webhook-replay.md](docs/webhook-replay.md) - Complete feature documentation
- [docs/audit_logging.md](docs/audit_logging.md) - Audit logging system
- [docs/idempotency.md](docs/idempotency.md) - Idempotency constraints
- [docs/webhook-handler.md](docs/webhook-handler.md) - Original webhook processing

## Contact

For questions or issues with this implementation, please refer to Issue #98 in the project tracker.
