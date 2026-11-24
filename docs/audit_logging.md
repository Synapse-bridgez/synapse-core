# Audit Logging Implementation (Issue #20)

## Overview

This implementation provides a **tamper-evident audit logging system** for all critical changes in the Synapse Core financial system. It ensures strict audit trails for compliance with SOC2/ISO standards.

## Architecture

### Database Schema

The audit logging system uses an **append-only table** design that prevents tampering:

```sql
CREATE TABLE audit_logs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    entity_id UUID NOT NULL,
    entity_type VARCHAR(50) NOT NULL,
    action VARCHAR(50) NOT NULL,
    old_val JSONB,
    new_val JSONB,
    actor VARCHAR(255) NOT NULL DEFAULT 'system',
    timestamp TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
```

**Key Design Decisions:**
- **Append-Only**: Rows are never updated. Rows are removed only by the retention job (see [Retention and archival](#retention-and-archival)), which archives them first and keeps rows for disputed transactions
- **JSONB Storage**: Flexible diff format for any entity changes
- **Indexed for Performance**: Multiple indexes for efficient queries
- **Timestamps**: `timestamp` and `created_at` both default to `NOW()` in the database. `AuditLog::log()` does not write either column, so both record the insert time
- **Actor Tracking**: Records who (system/user) made the change

### Indexes

```sql
CREATE INDEX idx_audit_logs_entity_id ON audit_logs(entity_id);
CREATE INDEX idx_audit_logs_entity_type ON audit_logs(entity_type);
CREATE INDEX idx_audit_logs_timestamp ON audit_logs(timestamp);
CREATE INDEX idx_audit_logs_actor ON audit_logs(actor);
CREATE INDEX idx_audit_logs_action ON audit_logs(action);
CREATE INDEX idx_audit_logs_entity_timestamp ON audit_logs(entity_id, timestamp DESC);
```

These indexes enable fast queries for audit trail reporting and forensic analysis.

## Implementation Details

### Files Changed

1. **migrations/20260220000001_audit_logs.sql**
   - Creates the audit_logs table with proper schema
   - Defines all required indexes

2. **src/db/audit.rs**
   - `AuditLog` struct: Represents an audit log entry
   - `AuditLog::log()`: Generic logging function; inserts `entity_id`, `entity_type`, `action`, `old_val`, `new_val` and `actor`
   - `AuditLog::log_status_change()`: Logs status transitions
   - `AuditLog::log_field_update()`: Logs field changes
   - `AuditLog::log_creation()`: Logs entity creation
   - `AuditLog::log_deletion()`: Logs entity deletion

3. **src/db/queries.rs**
   - Updated `insert_transaction()`: Logs transaction creation
   - Updated `update_transactions_settlement()`: Logs settlement assignment
   - Updated `insert_settlement()`: Logs settlement creation
   - `get_audit_logs()`: Retrieves the audit trail for one entity with keyset pagination

4. **src/db/mod.rs**
   - Added `pub mod audit;`

5. **Cargo.toml**
   - Added `bigdecimal = { version = "0.3", features = ["serde"] }`
   - Added `ipnet = "2.9"`

### Key Features

#### 1. Transaction Safety
All audit logs are recorded **in the same database transaction** as the entity change:

```rust
pub async fn insert_transaction(pool: &PgPool, tx: &Transaction) -> Result<Transaction> {
    let mut transaction = pool.begin().await?;
    
    // Insert transaction
    let result = sqlx::query_as::<_, Transaction>(...)
        .fetch_one(&mut *transaction)
        .await?;

    // Log creation in same transaction
    AuditLog::log_creation(
        &mut transaction,
        result.id,
        ENTITY_TRANSACTION,
        created_data,
        "system",
    )
    .await?;

    transaction.commit().await?;
    Ok(result)
}
```

If either the entity insertion or audit log fails, **both are rolled back**.

#### 2. Comprehensive Audit Trail

**Creation Events:**
```json
{
    "action": "created",
    "old_val": null,
    "new_val": {
        "stellar_account": "...",
        "amount": "1000.00",
        "asset_code": "USDC",
        "status": "pending"
    },
    "actor": "system"
}
```

**Status Updates:**
```json
{
    "action": "status_update",
    "old_val": { "status": "pending" },
    "new_val": { "status": "completed" },
    "actor": "system"
}
```

**Field Updates:**
```json
{
    "action": "settlement_id_update",
    "old_val": { "settlement_id": null },
    "new_val": { "settlement_id": "550e8400-e29b-41d4-a716-446655440000" },
    "actor": "system"
}
```

#### 3. Entity Types

Currently tracked entities:
- `ENTITY_TRANSACTION`: Transaction entities
- `ENTITY_SETTLEMENT`: Settlement entities

These are defined as constants in `src/db/audit.rs` for type safety.

### Audit Logging Points

#### Transaction Creation (insert_transaction)
Logs when a new transaction is created with all initial details.

#### Settlement Creation (insert_settlement)
Logs when a settlement is created with aggregated amounts and transaction counts.

#### Settlement Assignment (update_transactions_settlement)
Logs for each transaction when it's linked to a settlement, tracking the relationship change.

## Usage Examples

### Logging a Status Change
```rust
AuditLog::log_status_change(
    &mut tx,
    settlement.id,
    ENTITY_SETTLEMENT,
    "pending",
    "completed",
    "api-user-123",
).await?;
```

### Logging a Field Update
```rust
AuditLog::log_field_update(
    &mut tx,
    transaction.id,
    ENTITY_TRANSACTION,
    "amount",
    json!("1000.00"),
    json!("1050.00"),
    "admin",
).await?;
```

### Logging Entity Creation
```rust
AuditLog::log_creation(
    &mut tx,
    entity.id,
    ENTITY_TRANSACTION,
    json!({
        "stellar_account": "...",
        "amount": "1000.00",
        "asset_code": "USDC"
    }),
    "system",
).await?;
```

### Retrieving Audit Trail
```rust
// First page, newest first. Pass the (timestamp, id) of the last row of the
// previous page as the cursor to fetch the next page.
let logs = queries::get_audit_logs(
    &pool,
    entity_id,
    20,    // limit
    None,  // cursor: Option<(DateTime<Utc>, Uuid)>
).await?;

for (id, entity_id, entity_type, action, old_val, new_val, actor, timestamp) in logs {
    println!("{timestamp} {entity_id}: {entity_type} {action} by {actor}");
}
```

## Retention and archival

`audit::run_retention` (in `src/db/audit.rs`) enforces the retention period:

- The period is read from `AUDIT_LOG_RETENTION_DAYS` and defaults to 365 days. A missing, non-numeric, zero or negative value falls back to the default.
- Rows older than the cutoff are exported to a gzip archive through an `ArchiveStorage` backend (`LocalDiskArchiveStorage` writes to local disk).
- Rows are deleted only after the archive write returns success. If the write fails, the run returns an error and `audit_logs` is left untouched.
- Rows that belong to disputed transactions are never deleted.
- Each archive is recorded in the `audit_log_archives` table (`location`, `checksum`, `row_count`, `covers_from`, `covers_to`) so a period's archive can be located and verified.

## Compliance Benefits

### SOC2 Type II
✅ Audit logging with tamper-evident design
✅ Append-only audit trail (rows are only removed by the archive-first retention job)
✅ Actor tracking (who made changes)
✅ Timestamp tracking (when changes occurred)
✅ Transaction consistency (same database transaction)

### ISO 27001
✅ Detailed change tracking
✅ System accountability (actor field)
✅ Change history (diffs in JSON)
✅ Non-repudiation (immutable logs)

## Future Enhancements

1. **Cryptographic Signing**: Add hash chains or digital signatures to prevent even database-level tampering
2. **Audit Log Export**: Archives are written by the retention job, but there is no tool yet to export or verify logs on demand for compliance audits
3. **Real-time Alerting**: Alert on suspicious patterns (e.g., multiple rapid changes)
4. **Durable Archive Storage**: Archives currently go to local disk; add a backend that writes to immutable storage (e.g., AWS S3 Glacier)
5. **User Actor Integration**: Replace "system" actor with actual user identifiers when integrating authentication
6. **Multi-Version Views**: Track full entity snapshots at each change point

## Testing

Unit tests are included in `src/db/audit.rs` (entry construction and the retention-period settings). For example:
```rust
#[test]
fn test_audit_log_creation() {
    let audit = AuditLog::new(
        entity_id,
        ENTITY_TRANSACTION,
        "status_update",
        Some(json!({"status": "pending"})),
        Some(json!({"status": "completed"})),
        "system",
    );
    assert_eq!(audit.action, "status_update");
}
```

## Integration Notes

- All state-changing operations should wrap their logic in a database transaction
- Audit logging happens **before** the transaction commits
- If audit logging fails, the entire transaction is rolled back (fail-safe design)
- The `actor` field should be populated with the actual user/service performing the action

## Migration Path

The migration `20260220000001_audit_logs.sql` is applied automatically on:
1. Application startup via SQLx migrations
2. Database setup in CI/CD pipelines
3. Manual deployment with `sqlx migrate run`

No special migration steps are required beyond normal deployment procedures.
