# Transaction Processor Service Tests

This directory contains comprehensive unit and integration tests for the transaction processor service.

## Test Coverage

### 1. `test_process_transaction_success`

Tests the happy path where a transaction is successfully processed from pending to completed status.

**Verifies:**

- Transaction status transitions: Pending → Processing → Completed
- Stellar verification is called with correct transaction hash
- Repository updates are performed in correct order

### 2. `test_process_transaction_with_stellar_verification`

Tests transaction processing with explicit Stellar Horizon verification.

**Verifies:**

- Stellar client verification is invoked
- Transaction completes successfully after verification
- No unnecessary Stellar API calls are made

### 3. `test_process_transaction_error_handling`

Tests error handling when Stellar verification fails and transaction has exceeded retry limit.

**Verifies:**

- Failed transactions are moved to DLQ after max retries
- Transaction status is updated to InDLQ
- Error messages are properly propagated
- DLQ service receives correct transaction and error details

### 4. `test_requeue_from_dlq`

Tests the functionality to requeue transactions from the Dead Letter Queue.

**Verifies:**

- Transactions can be retrieved from DLQ
- Requeued transactions are processed normally
- Status transitions work correctly for requeued items

### 5. `test_concurrent_processing`

Tests concurrent processing of multiple transactions to ensure thread safety.

**Verifies:**

- Multiple transactions can be processed simultaneously
- No race conditions occur
- All transactions complete successfully
- Shared resources are properly synchronized

## Running Tests

```bash
# Run all tests
cargo test

# Run specific test
cargo test test_process_transaction_success

# Run with output
cargo test -- --nocapture

# Run with specific number of threads
cargo test -- --test-threads=1
```

## Mock Strategy

The tests use `mockall` to create deterministic mocks for:

- **StellarHorizonClient**: Mocks Stellar blockchain interactions
- **TransactionRepository**: Mocks database operations
- **DLQService**: Mocks Dead Letter Queue operations

This approach ensures:

- Tests run quickly without external dependencies
- Deterministic behavior for CI/CD pipelines
- Ability to test error scenarios that are hard to reproduce

## Implementation Notes

- All mocks are wrapped in trait implementations for dependency injection
- Tests use `tokio::test` for async execution
- Helper functions reduce test boilerplate
- Each test is isolated and can run independently
