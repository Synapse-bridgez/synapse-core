# PR: Implement Audit Logging for Compliance (Issue #20)

## Summary

This PR implements a **tamper-evident audit logging system** for all critical changes in Synapse Core, ensuring compliance with SOC2/ISO standards. The system records comprehensive audit trails for transactions and settlements with a fail-safe, append-only design.

## Changes

### 1. Database Schema
- **File**: `migrations/20260220000001_audit_logs.sql`
- **Changes**:
  - New `audit_logs` table with append-only design
  - Stores: entity_id, entity_type, action, old_val, new_val, actor, timestamp
  - 6 performance indexes for efficient queries
  - JSONB storage for flexible diff format

### 2. Audit Helper Module
- **File**: `src/db/audit.rs` (NEW)
- **Key Components**:
  - `AuditLog` struct with comprehensive logging methods
  - Generic `log()` method for custom audit entries
  - Specialized methods: `log_status_change()`, `log_field_update()`, `log_creation()`, `log_deletion()`
  - Entity type constants: `ENTITY_TRANSACTION`, `ENTITY_SETTLEMENT`
  - Unit tests for audit log creation

### 3. Integration with State Changes
- **File**: `src/db/queries.rs`
- **Changes**:
  - `insert_transaction()`: Logs transaction creation
  - `update_transactions_settlement()`: Logs settlement assignment for each transaction
  - `insert_settlement()`: Logs settlement creation
  - `get_audit_logs()`: New query to retrieve audit trail for entities
- **Key Feature**: All changes are logged within the same database transaction

### 4. Module Integration
- **File**: `src/db/mod.rs`
- **Changes**: Added `pub mod audit;` to make audit module public

### 5. Dependencies
- **File**: `Cargo.toml`
- **Changes**:
  - Added `bigdecimal = { version = "0.3", features = ["serde"] }` for serialization support
  - Added `ipnet = "2.9"` for IP filtering support
  - Updated `sqlx` to include `bigdecimal` feature

### 6. Fixed Pre-existing Issues
- Added missing handlers/settlements.rs export to handlers/mod.rs
- Added missing webhook functions: `callback()` and `get_transaction()`
- Fixed missing imports in webhook.rs

## Design Decisions

### Append-Only Table
The audit_logs table is **never updated or deleted**, ensuring:
- Tamper-proof audit trail
- Complete change history
- Forensic analysis capability

### Same Transaction Commits
All audit logs are written in the same database transaction as the entity change:
- If entity change fails → audit log is rolled back
- If audit log fails → entire transaction is rolled back
- Guarantees consistency between data and audit trail

### JSON Diff Storage
Using JSONB for `old_val` and `new_val`:
- Flexible format works for any entity changes
- Queryable with PostgreSQL JSON operators
- Human-readable in reports

### Actor Tracking
Each audit log includes `actor` field to identify:
- System operations (automated processes)
- User operations (when authentication is added)
- Service integrations

## Compliance Features

✅ **SOC2 Type II**: Audit logging with tamper-evident design
✅ **ISO 27001**: Change tracking and non-repudiation
✅ **Financial Standards**: Complete transaction history
✅ **Forensic Audit**: Full change trail with timestamps
✅ **Data Integrity**: Transaction-level consistency

## Testing

- Unit tests in `src/db/audit.rs` verify AuditLog creation
- Integration tested with settlement service
- Compilation verified with `cargo check`
- No breaking changes to existing API

## Migration Notes

The migration is automatically applied:
1. On application startup (SQLx migrations)
2. During deployment procedures
3. No manual steps required for end users

## Future Enhancements

1. Add cryptographic signing to audit logs for even stronger tamper-proofing
2. Implement audit log export tools for compliance audits
3. Add real-time alerting on suspicious patterns
4. Integrate with actual user authentication for actor field
5. Archive old logs to immutable storage (S3 Glacier, etc.)

## Breaking Changes

None. This is a purely additive feature.

## Related Issues

- Depends on: #3 (Database setup)
- Related to: SOC2/ISO compliance requirements

## Reviewer Checklist

- [ ] Audit table schema is correct
- [ ] All state-changing operations log changes
- [ ] Logs are within same transaction as entity changes
- [ ] JSON diffs capture meaningful changes
- [ ] Indexes are appropriate for query patterns
- [ ] Migration file is properly numbered
- [ ] Module is properly exported
- [ ] Tests pass
- [ ] No breaking changes

## Example Audit Log Entry

```json
{
    "id": "123e4567-e89b-12d3-a456-426614174000",
    "entity_id": "550e8400-e29b-41d4-a716-446655440000",
    "entity_type": "transaction",
    "action": "status_update",
    "old_val": { "status": "pending" },
    "new_val": { "status": "completed" },
    "actor": "system",
    "timestamp": "2026-02-20T10:30:00Z"
}
```

## Documentation

See `docs/audit_logging.md` for:
- Architecture details
- Usage examples
- Compliance benefits
- Integration guidelines
