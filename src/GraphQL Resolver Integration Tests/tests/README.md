# GraphQL Integration Tests

Comprehensive integration tests for GraphQL resolvers covering queries, mutations, and subscriptions.

## Test Coverage

### Query Tests

- `test_graphql_transaction_query` - Test single transaction retrieval
- `test_graphql_transaction_query_not_found` - Error handling for missing transactions
- `test_graphql_transactions_list` - List transactions with filters
- `test_graphql_transactions_list_with_date_filter` - Date range filtering
- `test_graphql_transactions_list_invalid_filter` - Invalid filter validation
- `test_graphql_settlement_query` - Single settlement retrieval
- `test_graphql_settlement_query_not_found` - Error handling for missing settlements

### Mutation Tests

- `test_graphql_force_complete_mutation` - Force complete transaction
- `test_graphql_force_complete_mutation_invalid_transaction` - Invalid transaction handling
- `test_graphql_force_complete_mutation_already_completed` - Already completed validation
- `test_graphql_dlq_replay_mutation` - Replay single DLQ message
- `test_graphql_dlq_replay_mutation_batch` - Batch DLQ replay
- `test_graphql_dlq_replay_mutation_not_found` - Missing DLQ message handling

### Subscription Tests

- `test_graphql_transaction_subscription` - Single transaction status subscription
- `test_graphql_transaction_subscription_multiple` - Multiple transaction subscriptions
- `test_graphql_transaction_subscription_invalid_id` - Invalid ID validation

### Integration Tests

- `test_graphql_transaction_query_with_relations` - Nested relations
- `test_graphql_transactions_list_pagination` - Pagination support
- `test_graphql_transactions_list_multiple_filters` - Complex filtering
- `test_graphql_settlement_query_with_transactions` - Settlement with nested data
- `test_graphql_force_complete_mutation_with_validation` - Input validation
- `test_graphql_dlq_replay_mutation_with_retry_count` - Retry tracking
- `test_graphql_concurrent_mutations` - Concurrent operation handling
- `test_graphql_error_handling_invalid_input` - Comprehensive error handling
- `test_graphql_authorization_scenarios` - Authorization checks

## Running Tests

```bash
# Run all tests
cargo test

# Run specific test
cargo test test_graphql_transaction_query

# Run with output
cargo test -- --nocapture

# Run tests in parallel
cargo test -- --test-threads=4
```

## Test Structure

Each test follows this pattern:

1. Setup test schema
2. Create GraphQL request with query/mutation/subscription
3. Execute request
4. Assert response (success or error scenarios)

## Notes

- Tests cover both successful and error scenarios as required
- Subscription tests validate query structure (full streaming tests require additional setup)
- Mock schema needs to be replaced with actual resolver implementations
- Authorization tests should be expanded based on actual auth implementation
