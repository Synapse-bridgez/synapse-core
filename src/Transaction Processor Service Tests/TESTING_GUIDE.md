# Transaction Processor Testing Guide

## Quick Start

```bash
# Run all tests
cargo test

# Run with output
cargo test -- --nocapture

# Run specific test
cargo test test_process_transaction_success
```

## Test Structure

### Test Organization

```
tests/
├── transaction_processor_test.rs  # Main test file
└── README.md                      # Test documentation
```

### Key Components

#### Mock Objects

- `MockStellarHorizonClient` - Simulates Stellar blockchain API
- `MockTransactionRepository` - Simulates database operations
- `MockDLQService` - Simulates Dead Letter Queue

#### Test Helpers

- `create_test_transaction()` - Creates test transaction instances
- `create_processor()` - Assembles processor with mocked dependencies

## Test Scenarios

### 1. Success Path

**Test:** `test_process_transaction_success`
**Scenario:** Transaction processes successfully from start to finish
**Expected:** Status transitions Pending → Processing → Completed

### 2. Stellar Verification

**Test:** `test_process_transaction_with_stellar_verification`
**Scenario:** Transaction verified through Stellar Horizon API
**Expected:** Verification succeeds, transaction completes

### 3. Error Handling

**Test:** `test_process_transaction_error_handling`
**Scenario:** Transaction fails after max retries
**Expected:** Transaction moved to DLQ, status updated to InDLQ

### 4. DLQ Requeue

**Test:** `test_requeue_from_dlq`
**Scenario:** Failed transaction requeued from DLQ
**Expected:** Transaction reprocessed successfully

### 5. Concurrent Processing

**Test:** `test_concurrent_processing`
**Scenario:** Multiple transactions processed simultaneously
**Expected:** All transactions complete without conflicts

## Adding New Tests

### Template

```rust
#[tokio::test]
async fn test_your_scenario() {
    // Arrange - Set up mocks and test data
    let tx_id = "tx_test";
    let mut mock_stellar = MockStellarHorizonClient::new();
    mock_stellar
        .expect_verify_transaction()
        .returning(|_| Ok(true));

    // ... configure other mocks

    // Act - Execute the test
    let processor = create_processor(mock_stellar, mock_repo, mock_dlq);
    let result = processor.process_transaction(tx_id).await;

    // Assert - Verify expectations
    assert!(result.is_ok());
}
```

### Best Practices

1. Use descriptive test names that explain the scenario
2. Follow Arrange-Act-Assert pattern
3. Mock only what's necessary for the test
4. Use `.times(n)` to verify exact call counts
5. Test both success and failure paths
6. Keep tests isolated and independent

## Debugging Tests

### View Mock Call Details

```bash
# Run with backtrace
RUST_BACKTRACE=1 cargo test

# Run single test with output
cargo test test_name -- --nocapture --test-threads=1
```

### Common Issues

#### Mock Expectation Failures

**Problem:** "Mock function called with unexpected arguments"
**Solution:** Check `.with()` predicates match actual call parameters

#### Async Runtime Issues

**Problem:** "Cannot start a runtime from within a runtime"
**Solution:** Use `#[tokio::test]` instead of `#[test]`

#### Call Count Mismatches

**Problem:** "Expected 1 call, got 2"
**Solution:** Review test logic, adjust `.times()` expectation

## CI/CD Integration

### GitHub Actions Example

```yaml
name: Tests
on: [push, pull_request]
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v2
      - uses: actions-rs/toolchain@v1
        with:
          toolchain: stable
      - run: cargo test --verbose
```

## Performance Considerations

- Tests run in ~100ms without external dependencies
- Concurrent test execution is safe (no shared state)
- Mock setup is lightweight and fast
- No network calls or database connections

## Maintenance

### Updating Mocks

When the actual service interface changes:

1. Update mock trait definitions
2. Update wrapper implementations
3. Adjust test expectations
4. Run full test suite to verify

### Adding Coverage

To add new test scenarios:

1. Identify untested code paths
2. Create new test function
3. Configure mocks for scenario
4. Add documentation to README
5. Update this guide

## Resources

- [mockall documentation](https://docs.rs/mockall/)
- [tokio testing guide](https://tokio.rs/tokio/topics/testing)
- [Rust testing best practices](https://doc.rust-lang.org/book/ch11-00-testing.html)
