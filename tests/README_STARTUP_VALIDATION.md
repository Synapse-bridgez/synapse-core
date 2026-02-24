# Startup Validation Integration Tests

## Overview

This test suite provides comprehensive integration testing for the startup validation workflow in `src/startup.rs`. The tests verify that the service correctly validates all dependencies before starting.

## Test Coverage

### 1. `test_validation_all_healthy`
Tests the happy path where all services are available and healthy:
- Database connectivity
- Redis connectivity  
- Horizon API connectivity
- Environment variable validation

### 2. `test_validation_database_unavailable`
Tests behavior when the database is unavailable:
- Verifies database validation fails
- Confirms error is reported in ValidationReport
- Ensures overall validation fails

### 3. `test_validation_redis_unavailable`
Tests behavior when Redis is unavailable:
- Verifies Redis validation fails
- Confirms error is reported in ValidationReport
- Ensures other services can still be validated independently

### 4. `test_validation_horizon_unavailable`
Tests behavior when Stellar Horizon is unavailable:
- Verifies Horizon validation fails
- Confirms error is reported in ValidationReport
- Tests with invalid/unreachable Horizon URL

### 5. `test_validation_report_generation`
Tests the ValidationReport structure and content:
- Verifies report correctly tracks individual service status
- Confirms error messages are descriptive
- Tests the `is_valid()` method
- Validates the `print()` method output

### 6. `test_validation_empty_database_url`
Tests environment validation with empty configuration:
- Verifies environment validation catches empty DATABASE_URL
- Confirms validation fails before attempting connection

### 7. `test_validation_invalid_horizon_url_format`
Tests environment validation with malformed URLs:
- Verifies URL format validation
- Confirms invalid URLs are caught early

### 8. `test_validation_multiple_failures`
Tests behavior with multiple simultaneous failures:
- Verifies all failures are detected and reported
- Confirms error messages for each failed service
- Tests that validation continues even after first failure

## Running the Tests

### Run all startup validation tests:
```bash
cargo test --test startup_validation_test
```

### Run with output visible:
```bash
cargo test --test startup_validation_test -- --nocapture
```

### Run a specific test:
```bash
cargo test --test startup_validation_test test_validation_all_healthy -- --nocapture
```

## Test Dependencies

The tests use:
- `testcontainers` - For spinning up real PostgreSQL instances
- `testcontainers-modules` - PostgreSQL module for testcontainers
- `sqlx` - For database operations and migrations
- `tokio` - Async runtime

## Notes

### Redis Testing
Some tests expect Redis to be unavailable (testing failure scenarios). For the `test_validation_all_healthy` test to fully pass, you may need:
- Redis running locally on port 6379, OR
- Modify the test to use testcontainers for Redis (requires adding testcontainers-modules Redis support)

### Horizon Testing
Tests use the public Stellar testnet Horizon API (`https://horizon-testnet.stellar.org`), which should be available without additional setup.

### Database Testing
All tests use testcontainers to spin up isolated PostgreSQL instances with migrations applied, ensuring clean test environments.

## CI/CD Considerations

These tests are suitable for CI/CD pipelines:
- Database tests use testcontainers (no external dependencies)
- Horizon tests use public testnet API
- Redis failure tests don't require Redis to be running
- Tests are isolated and can run in parallel

For full integration testing in CI, consider:
- Adding Redis via testcontainers or Docker Compose
- Setting appropriate timeouts for network calls
- Using test fixtures for consistent test data

## Future Enhancements

Potential improvements:
1. Add testcontainers support for Redis
2. Mock Horizon API responses for faster, more reliable tests
3. Add performance benchmarks for validation speed
4. Test validation with database replica failover
5. Add tests for concurrent validation calls
