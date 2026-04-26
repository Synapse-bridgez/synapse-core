# Database Pool Manager with Failover Tests

This project implements a database pool manager with primary/replica failover capabilities and comprehensive integration tests.

## Features

- Primary/replica connection routing
- Automatic failover when replicas fail
- Health check monitoring
- Concurrent query handling

## Running Tests

```bash
# Run all tests
cargo test

# Run specific test
cargo test test_read_query_routes_to_replica

# Run with output
cargo test -- --nocapture
```

## Test Coverage

- `test_read_query_routes_to_replica`: Verifies read queries route to replica
- `test_write_query_routes_to_primary`: Verifies write queries route to primary
- `test_failover_on_replica_failure`: Tests automatic failover when replica fails
- `test_pool_health_checks`: Tests connection pool health monitoring
- `test_concurrent_query_routing`: Tests concurrent query routing under load

## Implementation

The pool manager (`src/db/pool_manager.rs`) provides:

- Separate connection pools for primary and replica databases
- Query type routing (read vs write)
- Automatic failover to primary when replicas are unavailable
- Health check functionality

Tests use testcontainers to spin up real PostgreSQL instances for integration testing.
