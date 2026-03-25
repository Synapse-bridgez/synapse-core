# Design Document: Webhook Replay Admin Interface

## Overview

The webhook replay admin interface provides operators with the ability to reprocess historical webhook payloads that failed during initial processing. This system is critical for recovering from transient failures, testing bug fixes, and handling scenarios where processing logic has been updated after the original webhook was received.

The design builds upon the existing webhook processing infrastructure and audit logging system. It introduces three primary capabilities:

1. **Query Interface**: List and filter failed webhook attempts with rich metadata
2. **Replay Operations**: Execute single or batch replays with dry-run testing support
3. **Audit Trail**: Comprehensive tracking of all replay attempts with operator attribution

The system respects idempotency constraints to prevent duplicate processing side effects while providing operators with override capabilities when necessary. All operations require admin authentication and are fully audited for compliance and debugging purposes.

### Key Design Principles

- **Safety First**: Dry-run mode and idempotency checks prevent accidental duplicate processing
- **Auditability**: Every replay attempt is logged with operator identity and outcome
- **Performance**: Batch operations support efficient recovery from widespread failures
- **Simplicity**: Leverage existing transaction and audit log infrastructure

## Architecture

### System Context

The webhook replay system operates within the existing Synapse payment processing architecture:

```
┌─────────────────────────────────────────────────────────────┐
│                     External Anchor System                   │
└────────────────────────┬────────────────────────────────────┘
                         │ Original Webhooks
                         ▼
┌─────────────────────────────────────────────────────────────┐
│                   Webhook Processing Pipeline                │
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐  │
│  │   Receive    │───▶│   Process    │───▶│    Store     │  │
│  │   Webhook    │    │   Payload    │    │  Transaction │  │
│  └──────────────┘    └──────────────┘    └──────────────┘  │
│         │                    │                    │          │
│         └────────────────────┴────────────────────┘          │
│                              │                               │
│                              ▼                               │
│                     ┌──────────────────┐                     │
│                     │   Audit Logs     │                     │
│                     │  (Payload Store) │                     │
│                     └──────────────────┘                     │
└─────────────────────────────────────────────────────────────┘
                         │
                         │ Admin Operations
                         ▼
┌─────────────────────────────────────────────────────────────┐
│              Webhook Replay Admin Interface                  │
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐  │
│  │     List     │    │    Replay    │    │    Track     │  │
│  │    Failed    │    │   Webhooks   │    │   History    │  │
│  │   Webhooks   │    │ (Single/Batch)│   │              │  │
│  └──────────────┘    └──────────────┘    └──────────────┘  │
└─────────────────────────────────────────────────────────────┘
```

### Component Architecture

The replay system consists of four primary components:

1. **Query Handler**: Retrieves failed webhook attempts from the database with filtering and pagination
2. **Replay Orchestrator**: Coordinates replay operations, manages dry-run mode, and enforces idempotency
3. **Replay Tracker**: Records all replay attempts in the audit trail
4. **Authentication Layer**: Validates admin credentials and extracts operator identity

### Data Flow

#### Single Webhook Replay Flow

```
Admin Request
     │
     ▼
┌─────────────────┐
│  Authenticate   │
│   & Authorize   │
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│  Validate       │
│  Request Params │
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│  Retrieve       │
│  Transaction    │
│  from DB        │
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│  Check Status   │
│  & Idempotency  │
└────────┬────────┘
         │
         ▼
    ┌────────┐
    │Dry-Run?│
    └───┬────┘
        │
    ┌───┴───┐
    │       │
   Yes     No
    │       │
    │       ▼
    │  ┌─────────────────┐
    │  │  Update Status  │
    │  │  to 'pending'   │
    │  └────────┬────────┘
    │           │
    └───────────┤
                │
                ▼
       ┌─────────────────┐
       │  Track Replay   │
       │  in History     │
       └────────┬────────┘
                │
                ▼
       ┌─────────────────┐
       │  Return Result  │
       │  to Admin       │
       └─────────────────┘
```

#### Batch Replay Flow

Batch replays process each webhook sequentially, continuing even if individual replays fail. This ensures maximum recovery while providing detailed per-webhook results.

### Technology Stack

- **Language**: Rust
- **Web Framework**: Axum
- **Database**: PostgreSQL with SQLx
- **Authentication**: Existing admin_auth middleware
- **Serialization**: Serde JSON

## Components and Interfaces

### 1. Query Handler Component

**Responsibility**: Retrieve and filter failed webhook attempts

**Interface**:
```rust
pub async fn list_failed_webhooks(
    State(pool): State<PgPool>,
    Query(params): Query<ListFailedWebhooksQuery>,
) -> Result<impl IntoResponse, AppError>
```

**Input**:
```rust
pub struct ListFailedWebhooksQuery {
    pub limit: i64,           // Max 100, default 50
    pub offset: i64,          // Default 0
    pub asset_code: Option<String>,
    pub from_date: Option<DateTime<Utc>>,
    pub to_date: Option<DateTime<Utc>>,
}
```

**Output**:
```rust
pub struct FailedWebhooksResponse {
    pub total: i64,
    pub webhooks: Vec<FailedWebhookInfo>,
}

pub struct FailedWebhookInfo {
    pub transaction_id: Uuid,
    pub stellar_account: String,
    pub amount: String,
    pub asset_code: String,
    pub anchor_transaction_id: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub last_error: Option<String>,
    pub retry_count: i32,
}
```

**Query Logic**:
- Joins `transactions` table with `transaction_dlq` (dead letter queue)
- Filters by status='failed' OR presence in DLQ
- Applies optional filters (asset_code, date range)
- Orders by created_at DESC
- Supports pagination with limit/offset

### 2. Single Replay Handler Component

**Responsibility**: Replay a single webhook by transaction ID

**Interface**:
```rust
pub async fn replay_webhook(
    State(pool): State<PgPool>,
    Path(transaction_id): Path<Uuid>,
    Json(request): Json<ReplayWebhookRequest>,
) -> Result<impl IntoResponse, AppError>
```

**Input**:
```rust
pub struct ReplayWebhookRequest {
    pub dry_run: bool,  // Default false
}
```

**Output**:
```rust
pub struct ReplayResult {
    pub transaction_id: Uuid,
    pub success: bool,
    pub message: String,
    pub dry_run: bool,
    pub replayed_at: Option<DateTime<Utc>>,
}
```

**Processing Logic**:
1. Retrieve transaction from database
2. Validate transaction exists (404 if not found)
3. Check if transaction is completed (reject non-dry-run replays)
4. If dry-run: validate payload and return success without changes
5. If actual replay: update status to 'pending' for reprocessing
6. Track replay attempt in webhook_replay_history
7. Log replay in audit_logs table
8. Return result with success/failure status

### 3. Batch Replay Handler Component

**Responsibility**: Replay multiple webhooks in a single operation

**Interface**:
```rust
pub async fn batch_replay_webhooks(
    State(pool): State<PgPool>,
    Json(request): Json<BatchReplayRequest>,
) -> Result<impl IntoResponse, AppError>
```

**Input**:
```rust
pub struct BatchReplayRequest {
    pub transaction_ids: Vec<Uuid>,  // Max 1000
    pub dry_run: bool,
}
```

**Output**:
```rust
pub struct BatchReplayResponse {
    pub total: usize,
    pub successful: usize,
    pub failed: usize,
    pub results: Vec<ReplayResult>,
}
```

**Processing Logic**:
1. Validate batch size (max 1000 transaction IDs)
2. Iterate through each transaction ID sequentially
3. For each transaction:
   - Retrieve from database
   - Validate and check status
   - Execute replay (dry-run or actual)
   - Track result
   - Continue even if individual replay fails
4. Aggregate results (total, successful, failed counts)
5. Return comprehensive batch response

### 4. Replay Tracker Component

**Responsibility**: Record all replay attempts for audit trail

**Interface**:
```rust
async fn track_replay_attempt(
    pool: &PgPool,
    transaction_id: Uuid,
    dry_run: bool,
    success: bool,
    error_message: Option<String>,
) -> Result<(), AppError>
```

**Storage**:
Inserts record into `webhook_replay_history` table with:
- transaction_id: Reference to original transaction
- replayed_by: Operator identity (currently hardcoded as "admin")
- dry_run: Boolean flag
- success: Boolean outcome
- error_message: Optional error details
- replayed_at: Timestamp of replay attempt

### 5. Reprocessing Component

**Responsibility**: Execute the actual webhook reprocessing

**Interface**:
```rust
async fn reprocess_webhook(
    pool: &PgPool,
    transaction: &Transaction,
) -> Result<(), AppError>
```

**Processing Logic**:
- Updates transaction status from 'failed' to 'pending'
- Sets updated_at timestamp
- Allows existing webhook processing pipeline to pick up the transaction
- Respects idempotency keys through existing transaction state

### API Endpoints

All endpoints are mounted under `/admin/webhooks` and require admin authentication.

| Method | Path | Handler | Description |
|--------|------|---------|-------------|
| GET | `/admin/webhooks/failed` | list_failed_webhooks | Query failed webhooks with filters |
| POST | `/admin/webhooks/replay/:id` | replay_webhook | Replay single webhook |
| POST | `/admin/webhooks/replay/batch` | batch_replay_webhooks | Replay multiple webhooks |

### Authentication Integration

All endpoints use the existing `admin_auth` middleware which:
- Validates authentication credentials
- Verifies admin role/permissions
- Returns 401 Unauthorized for missing/invalid credentials
- Returns 403 Forbidden for insufficient permissions
- Extracts operator identity for audit logging

## Data Models

### Existing Tables (Used by Replay System)

#### transactions
Primary table storing all webhook-derived transactions:

```sql
CREATE TABLE transactions (
    id UUID PRIMARY KEY,
    stellar_account VARCHAR(56) NOT NULL,
    amount NUMERIC(19, 7) NOT NULL,
    asset_code VARCHAR(12) NOT NULL,
    anchor_transaction_id VARCHAR(255),
    transaction_type VARCHAR(50),
    status VARCHAR(50) NOT NULL,
    callback_url TEXT,
    memo TEXT,
    memo_type VARCHAR(50),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
```

**Key Fields for Replay**:
- `id`: Unique identifier used for replay operations
- `status`: Current processing state ('pending', 'completed', 'failed')
- `anchor_transaction_id`: Original webhook identifier
- All fields preserved for complete payload reconstruction

#### transaction_dlq
Dead Letter Queue for failed transactions:

```sql
CREATE TABLE transaction_dlq (
    id UUID PRIMARY KEY,
    transaction_id UUID NOT NULL REFERENCES transactions(id),
    error_reason TEXT,
    retry_count INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
```

**Usage in Replay**:
- Provides error context for failed webhooks
- Tracks retry attempts
- Used in listing failed webhooks query

#### audit_logs
Existing audit trail system:

```sql
CREATE TABLE audit_logs (
    id UUID PRIMARY KEY,
    entity_id UUID NOT NULL,
    entity_type VARCHAR(50) NOT NULL,
    action VARCHAR(100) NOT NULL,
    old_value JSONB,
    new_value JSONB,
    actor VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
```

**Usage in Replay**:
- Records webhook_replayed actions
- Stores before/after transaction state
- Captures operator identity

### New Table (Created for Replay System)

#### webhook_replay_history
Dedicated tracking table for replay operations:

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

CREATE INDEX idx_webhook_replay_history_transaction_id 
    ON webhook_replay_history(transaction_id);
CREATE INDEX idx_webhook_replay_history_replayed_at 
    ON webhook_replay_history(replayed_at DESC);
CREATE INDEX idx_webhook_replay_history_success 
    ON webhook_replay_history(success);
```

**Purpose**:
- Separate from audit_logs for specialized replay queries
- Optimized indexes for common access patterns
- Simplified schema for replay-specific data

**Key Fields**:
- `transaction_id`: Links to original transaction
- `replayed_by`: Operator who initiated replay (for accountability)
- `dry_run`: Distinguishes test runs from actual replays
- `success`: Quick filter for failed replay attempts
- `error_message`: Debugging information for failures
- `replayed_at`: Temporal ordering of replay attempts

### Data Relationships

```
transactions (1) ──────── (0..1) transaction_dlq
     │
     │
     ├──────── (0..*) audit_logs
     │
     │
     └──────── (0..*) webhook_replay_history
```

- One transaction may have zero or one DLQ entry
- One transaction may have multiple audit log entries
- One transaction may have multiple replay history entries (multiple replay attempts)

### Idempotency Handling

The system respects idempotency through transaction status:

1. **Completed Transactions**: Cannot be replayed without dry-run mode
   - Prevents duplicate processing of successful webhooks
   - Dry-run mode allows testing without side effects

2. **Failed/Pending Transactions**: Can be replayed freely
   - Status update to 'pending' triggers reprocessing
   - Existing webhook pipeline handles idempotency keys

3. **Force Replay Option**: Future enhancement
   - Would bypass status checks
   - Requires explicit operator acknowledgment
   - Must be tracked in replay history


## Correctness Properties

*A property is a characteristic or behavior that should hold true across all valid executions of a system—essentially, a formal statement about what the system should do. Properties serve as the bridge between human-readable specifications and machine-verifiable correctness guarantees.*

### Property Reflection

After analyzing all acceptance criteria, I identified several areas of redundancy:

**Redundancy Group 1: Complete Webhook Storage**
- Criteria 1.1-1.5 all test that different fields are stored in the audit log
- These can be combined into a single comprehensive property about complete webhook storage

**Redundancy Group 2: Original Data Retrieval**
- Criteria 3.2 and 3.3 both test retrieving original data from audit logs
- These can be combined into a single property about complete data retrieval

**Redundancy Group 3: Audit Logging Fields**
- Criteria 6.1-6.5 all test that different fields are stored in replay history
- These can be combined into a single comprehensive property about complete replay tracking

**Redundancy Group 4: Audit Record Updates**
- Criteria 6.6 and 6.7 both test updating replay records with completion data
- These can be combined into a single property

**Redundancy Group 5: Idempotency Preservation**
- Criteria 3.4 and 7.1 are identical (both test using original idempotency key)
- Criterion 5.6 and 7.6 are identical (both test dry-run doesn't update idempotency state)
- These duplicates will be consolidated

**Redundancy Group 6: Not Found Errors**
- Criteria 3.8 and 9.1 are identical (both test 404 for non-existent webhooks)
- These will be consolidated

After reflection, the following properties provide unique validation value:

### Property 1: Complete Webhook Storage

*For any* webhook payload received by the system, storing it in the audit log should preserve the complete original payload, request headers, timestamp, idempotency key, and processing status.

**Validates: Requirements 1.1, 1.2, 1.3, 1.4, 1.5**

### Property 2: Status Filter Correctness

*For any* status filter value, all webhook attempts returned by the list endpoint should have a status matching the specified filter.

**Validates: Requirements 2.2**

### Property 3: Date Range Filter Correctness

*For any* date range filter (from_date, to_date), all webhook attempts returned by the list endpoint should have timestamps within the specified range (inclusive).

**Validates: Requirements 2.3**

### Property 4: Idempotency Key Filter Correctness

*For any* idempotency key filter, all webhook attempts returned by the list endpoint should have an idempotency key matching the specified filter.

**Validates: Requirements 2.4**

### Property 5: Timestamp Ordering

*For any* query to the list failed webhooks endpoint, the returned webhook attempts should be sorted by timestamp in descending order (newest first).

**Validates: Requirements 2.5**

### Property 6: Pagination Correctness

*For any* valid limit and offset values, the list endpoint should return at most 'limit' results starting from position 'offset', and requesting consecutive pages should not duplicate or skip results.

**Validates: Requirements 2.6**

### Property 7: Response Field Completeness

*For any* webhook attempt returned by the list endpoint, the response should include transaction_id, stellar_account, amount, asset_code, anchor_transaction_id, status, created_at, last_error, and retry_count fields.

**Validates: Requirements 2.7**

### Property 8: Original Data Retrieval

*For any* replay request for an existing webhook, the system should retrieve the complete original payload and headers from the audit log.

**Validates: Requirements 3.2, 3.3**

### Property 9: Idempotency Key Preservation

*For any* replayed webhook, the system should use the original idempotency key from the audit log, not generate a new one.

**Validates: Requirements 3.4, 7.1**

### Property 10: Replay Audit Logging

*For any* completed replay operation, the system should record a replay result entry in the audit log.

**Validates: Requirements 3.6**

### Property 11: Replay Response Presence

*For any* replay operation (successful or failed), the system should return a ReplayResult to the caller containing transaction_id, success status, message, dry_run flag, and replayed_at timestamp.

**Validates: Requirements 3.7**

### Property 12: Not Found Error Handling

*For any* replay request with a non-existent webhook ID, the system should return an HTTP 404 Not Found error with a descriptive message indicating the webhook was not found.

**Validates: Requirements 3.8, 9.1**

### Property 13: Batch Error Resilience

*For any* batch replay operation where some individual replays fail, the system should continue processing all remaining webhooks in the batch and not abort early.

**Validates: Requirements 4.3**

### Property 14: Batch Summary Correctness

*For any* batch replay operation, the response summary counts (total, successful, failed) should sum correctly: total = successful + failed, and total should equal the number of transaction IDs in the request.

**Validates: Requirements 4.4**

### Property 15: Batch Result Completeness

*For any* batch replay request with N transaction IDs, the response should contain exactly N individual ReplayResult entries, one for each transaction ID in the request.

**Validates: Requirements 4.5**

### Property 16: Batch Non-Existent ID Handling

*For any* batch replay containing non-existent transaction IDs, those specific replays should be marked as failed in the results, but processing should continue for all other IDs in the batch.

**Validates: Requirements 4.7**

### Property 17: Dry-Run State Preservation

*For any* replay operation with dry_run=true, the database state (transaction status, idempotency tracking) should remain unchanged after the operation completes.

**Validates: Requirements 5.2, 5.6, 7.6**

### Property 18: Dry-Run Response Format Consistency

*For any* replay operation, the ReplayResult response structure should be identical whether dry_run is true or false (same fields present).

**Validates: Requirements 5.3**

### Property 19: Dry-Run Flag Indication

*For any* replay operation with dry_run=true, the returned ReplayResult should have the dry_run field set to true.

**Validates: Requirements 5.4**

### Property 20: Dry-Run Audit Logging

*For any* dry-run replay operation, the system should record the attempt in the replay history table with dry_run=true.

**Validates: Requirements 5.5**

### Property 21: Complete Replay Tracking

*For any* replay operation initiated, the system should record an entry in webhook_replay_history containing transaction_id, replayed_by (operator identity), timestamp, dry_run flag, and the original webhook ID.

**Validates: Requirements 6.1, 6.2, 6.3, 6.4, 6.5**

### Property 22: Replay Completion Updates

*For any* completed replay operation, the system should update the replay history record with the final success status and any error messages.

**Validates: Requirements 6.6, 6.7**

### Property 23: Replay History Operator Filter

*For any* query to replay history with an operator filter, all returned replay attempts should have been initiated by the specified operator.

**Validates: Requirements 6.9**

### Property 24: Replay History Webhook Filter

*For any* query to replay history with a webhook ID filter, all returned replay attempts should reference the specified webhook ID.

**Validates: Requirements 6.10**

### Property 25: Idempotency Check Execution

*For any* replay of a webhook with a completed status, the system should check the idempotency key state before processing (unless force replay is enabled).

**Validates: Requirements 7.2**

### Property 26: Idempotency Skip Behavior

*For any* webhook replay where the idempotency key has already been successfully processed and the transaction is completed, the system should skip reprocessing and return a cached result (or reject the replay if not in dry-run mode).

**Validates: Requirements 7.3**

### Property 27: Idempotency Process Behavior

*For any* webhook replay where the idempotency key has not been successfully processed (transaction is failed or pending), the system should process the webhook payload.

**Validates: Requirements 7.4**

### Property 28: Idempotency State Update

*For any* successful replay operation (dry_run=false), the system should update the idempotency key tracking state to reflect the successful processing.

**Validates: Requirements 7.5**

### Property 29: Authentication Requirement

*For any* request to an admin endpoint without valid authentication credentials, the system should return an HTTP 401 Unauthorized error.

**Validates: Requirements 8.1, 8.2**

### Property 30: Authorization Requirement

*For any* authenticated request to an admin endpoint where the user lacks administrator privileges, the system should return an HTTP 403 Forbidden error.

**Validates: Requirements 8.3, 8.4**

### Property 31: Operator Identity Extraction

*For any* authenticated replay request, the system should extract and record the operator identity in the replay history and audit logs.

**Validates: Requirements 8.5**

### Property 32: Invalid Parameter Validation

*For any* replay request with invalid parameters (e.g., malformed UUID, invalid dry_run value), the system should return an HTTP 400 Bad Request error with validation details.

**Validates: Requirements 9.2**

### Property 33: Batch Size Validation

*For any* batch replay request with more than 1000 transaction IDs, the system should return an HTTP 400 Bad Request error indicating the batch size limit.

**Validates: Requirements 9.3, 4.6**

### Property 34: Processing Error Handling

*For any* replay operation that fails due to a processing error (database error, network error, etc.), the system should return an HTTP 500 Internal Server Error with error details.

**Validates: Requirements 9.4**

### Property 35: Error Message Inclusion

*For any* failed replay operation, the ReplayResult should include the original error message in the message field.

**Validates: Requirements 9.5**

### Property 36: Error Context Audit Logging

*For any* failed replay operation, the system should record the error message and context in the webhook_replay_history table.

**Validates: Requirements 9.6**

### Property 37: UUID Format Validation

*For any* replay request with a transaction ID that is not a valid UUID format, the system should reject the request with a validation error before attempting to query the database.

**Validates: Requirements 9.7**

## Error Handling

The webhook replay system implements comprehensive error handling across multiple layers:

### Input Validation Errors (HTTP 400)

**Invalid UUID Format**:
- Detected before database queries
- Returns descriptive error message
- Example: "Invalid transaction ID format: expected UUID"

**Invalid Parameters**:
- Dry-run flag must be boolean
- Limit must be positive integer (max 100)
- Offset must be non-negative integer
- Date ranges must be valid ISO 8601 timestamps

**Batch Size Exceeded**:
- Maximum 1000 transaction IDs per batch
- Returns error: "Batch size exceeds maximum limit of 1000"

### Authentication/Authorization Errors

**HTTP 401 Unauthorized**:
- Missing authentication credentials
- Invalid or expired authentication token
- Returns: "Authentication required"

**HTTP 403 Forbidden**:
- Valid authentication but insufficient privileges
- User lacks admin role
- Returns: "Administrator privileges required"

### Resource Not Found Errors (HTTP 404)

**Transaction Not Found**:
- Transaction ID doesn't exist in database
- Returns: "Transaction {id} not found"
- Applies to both single and batch replays
- In batch mode: marked as failed, processing continues

### Business Logic Errors (HTTP 400)

**Cannot Replay Completed Transaction**:
- Transaction status is 'completed'
- Replay requested with dry_run=false
- Returns: "Cannot replay completed transaction without dry-run mode"
- Rationale: Prevents accidental duplicate processing

### Processing Errors (HTTP 500)

**Database Errors**:
- Connection failures
- Query execution errors
- Transaction commit failures
- Returns: "Database error: {details}"
- Logged with full stack trace

**Unexpected Errors**:
- Serialization failures
- Unexpected state conditions
- Returns: "Internal server error: {details}"
- Logged for debugging

### Error Handling in Batch Operations

Batch replays implement fail-safe error handling:

1. **Individual Failure Isolation**: One failed replay doesn't abort the batch
2. **Detailed Error Reporting**: Each failed replay includes specific error message
3. **Summary Statistics**: Response includes total, successful, and failed counts
4. **Partial Success Support**: Batch can succeed partially (some pass, some fail)

Example batch response with mixed results:
```json
{
  "total": 3,
  "successful": 2,
  "failed": 1,
  "results": [
    {
      "transaction_id": "uuid-1",
      "success": true,
      "message": "Webhook replayed successfully",
      "dry_run": false,
      "replayed_at": "2026-02-23T11:00:00Z"
    },
    {
      "transaction_id": "uuid-2",
      "success": false,
      "message": "Transaction uuid-2 not found",
      "dry_run": false,
      "replayed_at": null
    },
    {
      "transaction_id": "uuid-3",
      "success": true,
      "message": "Webhook replayed successfully",
      "dry_run": false,
      "replayed_at": "2026-02-23T11:00:01Z"
    }
  ]
}
```

### Error Logging and Observability

All errors are logged with appropriate severity levels:

**ERROR Level**:
- Database connection failures
- Unexpected processing errors
- Authentication/authorization failures

**WARN Level**:
- Transaction not found (may be expected)
- Replay of completed transaction attempted
- Batch size limit exceeded

**INFO Level**:
- Successful replay operations
- Dry-run executions
- Query operations

Each log entry includes:
- Transaction ID (when applicable)
- Operator identity
- Error message and context
- Timestamp
- Request parameters

### Retry and Recovery

**No Automatic Retries**: Replay operations do not automatically retry on failure. This is intentional:
- Operators should investigate failures before retrying
- Prevents cascading failures
- Allows for manual intervention and debugging

**Manual Retry**: Operators can manually retry failed replays:
- Review error message in replay history
- Address underlying issue
- Submit new replay request

**Idempotency Protection**: Even with manual retries, idempotency keys prevent duplicate processing of successfully completed webhooks.

## Testing Strategy

The webhook replay system requires comprehensive testing across multiple dimensions to ensure correctness, reliability, and safety. We employ a dual testing approach combining property-based testing for universal correctness guarantees with unit testing for specific examples and edge cases.

### Testing Approach

**Property-Based Testing**: Validates universal properties across all inputs
- Generates random test data (transaction IDs, payloads, filters)
- Executes properties 100+ times per test
- Catches edge cases that manual testing might miss
- Provides strong correctness guarantees

**Unit Testing**: Validates specific examples and integration points
- Tests concrete scenarios with known inputs/outputs
- Validates error conditions with specific error messages
- Tests integration between components
- Provides regression protection

### Property-Based Testing Configuration

**Framework**: Use `proptest` crate for Rust property-based testing

**Configuration**:
```rust
proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]
    // Test cases here
}
```

**Test Tagging**: Each property test must reference its design property:
```rust
// Feature: webhook-replay-admin-interface, Property 1: Complete Webhook Storage
#[test]
fn prop_complete_webhook_storage() {
    // Test implementation
}
```

### Property Test Implementation Examples

**Property 2: Status Filter Correctness**
```rust
// Feature: webhook-replay-admin-interface, Property 2: Status Filter Correctness
#[proptest]
fn prop_status_filter_correctness(
    #[strategy(webhook_status_strategy())] status: String,
    #[strategy(vec(transaction_strategy(), 0..50))] transactions: Vec<Transaction>
) {
    // Setup: Insert transactions with various statuses
    // Execute: Query with status filter
    // Assert: All returned transactions have matching status
}
```

**Property 14: Batch Summary Correctness**
```rust
// Feature: webhook-replay-admin-interface, Property 14: Batch Summary Correctness
#[proptest]
fn prop_batch_summary_correctness(
    #[strategy(vec(any::<Uuid>(), 1..100))] transaction_ids: Vec<Uuid>
) {
    // Setup: Create mix of valid and invalid transaction IDs
    // Execute: Batch replay
    // Assert: total == successful + failed
    // Assert: total == transaction_ids.len()
}
```

**Property 17: Dry-Run State Preservation**
```rust
// Feature: webhook-replay-admin-interface, Property 17: Dry-Run State Preservation
#[proptest]
fn prop_dry_run_state_preservation(
    #[strategy(transaction_strategy())] transaction: Transaction
) {
    // Setup: Record initial database state
    // Execute: Dry-run replay
    // Assert: Database state unchanged (status, idempotency, etc.)
}
```

### Unit Test Coverage

**Endpoint Existence Tests**:
- Verify `/admin/webhooks/failed` endpoint exists and responds
- Verify `/admin/webhooks/replay/:id` endpoint exists
- Verify `/admin/webhooks/replay/batch` endpoint exists

**Specific Error Condition Tests**:
- Test 404 error for non-existent transaction ID
- Test 401 error for missing authentication
- Test 403 error for non-admin user
- Test 400 error for invalid UUID format
- Test 400 error for batch size > 1000

**Integration Tests**:
- Test complete replay flow: list failed → replay → verify status change
- Test dry-run doesn't affect database state
- Test batch replay with mixed success/failure
- Test audit logging integration
- Test replay history tracking

**Serialization Tests**:
- Test ReplayResult JSON serialization
- Test BatchReplayResponse JSON serialization
- Test FailedWebhooksResponse JSON serialization

### Test Data Generators

**Transaction Generator**:
```rust
fn transaction_strategy() -> impl Strategy<Value = Transaction> {
    (
        stellar_account_strategy(),
        amount_strategy(),
        asset_code_strategy(),
        option::of(any::<String>()),  // anchor_transaction_id
        webhook_status_strategy(),
    ).prop_map(|(account, amount, asset, anchor_id, status)| {
        Transaction::new(account, amount, asset, anchor_id, 
                        Some("deposit".to_string()), 
                        Some(status), None, None, None)
    })
}
```

**Webhook Status Generator**:
```rust
fn webhook_status_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("pending".to_string()),
        Just("completed".to_string()),
        Just("failed".to_string()),
    ]
}
```

**UUID Generator**:
```rust
fn uuid_strategy() -> impl Strategy<Value = Uuid> {
    any::<[u8; 16]>().prop_map(Uuid::from_bytes)
}
```

### Test Database Setup

**Test Isolation**: Each test uses a separate database transaction that rolls back after completion

**Test Fixtures**:
- Create helper functions for common test data setup
- Provide builders for Transaction, ReplayRequest, BatchReplayRequest
- Mock authentication middleware for admin tests

**Database Migrations**: Run all migrations before test suite execution

### Performance Testing

While not part of correctness properties, performance should be validated:

**Single Replay Performance**:
- Target: < 5 seconds
- Test with various transaction sizes
- Monitor database query performance

**Batch Replay Performance**:
- Target: < 60 seconds for 100 webhooks
- Test with batches of 10, 50, 100, 500, 1000
- Monitor memory usage and database connection pool

**Query Performance**:
- Target: < 500ms for listing up to 100 results
- Test with various filter combinations
- Test with large datasets (10k+ transactions)

### Security Testing

**Authentication Tests**:
- Verify all endpoints reject unauthenticated requests
- Verify token validation works correctly
- Test expired token handling

**Authorization Tests**:
- Verify non-admin users cannot access endpoints
- Test role-based access control
- Verify operator identity extraction

**Input Validation Tests**:
- Test SQL injection prevention (parameterized queries)
- Test XSS prevention in error messages
- Test UUID validation prevents injection

### Test Execution

**Continuous Integration**:
- Run all tests on every commit
- Run property tests with 100 iterations minimum
- Fail build on any test failure

**Local Development**:
```bash
# Run all tests
cargo test

# Run only property tests
cargo test prop_

# Run with verbose output
cargo test -- --nocapture

# Run specific test
cargo test test_replay_webhook_tracking
```

**Test Coverage**:
- Target: > 80% code coverage
- Use `cargo tarpaulin` for coverage reporting
- Focus on critical paths (replay logic, error handling)

### Test Maintenance

**Property Test Failures**:
- When a property test fails, it provides a minimal failing example
- Add the failing case as a unit test for regression protection
- Fix the underlying bug
- Re-run property test to verify fix

**Flaky Tests**:
- Property tests should be deterministic (use seeded RNG if needed)
- Database tests should be properly isolated
- Avoid time-dependent assertions

**Test Documentation**:
- Each property test includes a comment linking to the design property
- Complex test setups include explanatory comments
- Test names clearly describe what is being tested
