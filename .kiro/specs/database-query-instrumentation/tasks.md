# Implementation Plan: Database Query Instrumentation

## Overview

This plan implements database query instrumentation for a Rust-based payment processing system using sqlx with PostgreSQL. The implementation adds timing measurement, slow query logging, optional Prometheus metrics, and debug mode support with minimal overhead (< 1ms per query).

## Tasks

- [ ] 1. Extend configuration module with instrumentation settings
  - [ ] 1.1 Add instrumentation configuration fields to src/config.rs
    - Add `slow_query_threshold_ms: u64` field (default: 100)
    - Add `db_log_all_queries: bool` field (default: false)
    - Add `enable_db_metrics: bool` field (default: false)
    - Implement environment variable loading for new fields
    - Add validation for positive threshold values
    - _Requirements: 2.1, 3.1, 3.2, 8.1, 8.2, 8.3, 8.4, 8.5_

  - [ ]* 1.2 Write unit tests for configuration loading
    - Test default values are applied correctly
    - Test environment variable overrides work
    - Test invalid values trigger warnings and use defaults
    - _Requirements: 8.3, 8.4, 8.5_

- [ ] 2. Create instrumented database pool module
  - [ ] 2.1 Create src/db/instrumented.rs with InstrumentedPool struct
    - Define `InstrumentedPool` wrapping `sqlx::PgPool`
    - Add fields for configuration (threshold, log_all, metrics_enabled)
    - Add optional `MetricsExporter` field for Prometheus integration
    - Implement `new()` constructor accepting pool and config
    - Implement `Clone` trait for InstrumentedPool
    - _Requirements: 5.1, 5.3, 5.5_

  - [ ] 2.2 Implement timing measurement infrastructure
    - Create helper function to capture start time using `std::time::Instant`
    - Create helper function to calculate duration in milliseconds
    - Ensure overhead is minimal (< 1ms)
    - _Requirements: 1.1, 1.2, 1.3, 1.4, 1.5_

  - [ ] 2.3 Implement query logging functionality
    - Create `log_query()` helper function accepting query_name, duration, rows_affected
    - Implement slow query logging when duration exceeds threshold
    - Implement debug mode logging for all queries
    - Use efficient logging without cloning query strings
    - Include query_name, duration_ms, and rows_affected in logs
    - _Requirements: 2.2, 2.3, 2.4, 2.5, 2.6, 3.2, 3.3, 3.4, 3.5_

  - [ ]* 2.4 Write unit tests for logging functionality
    - Test slow query logging triggers correctly
    - Test debug mode logs all queries
    - Test normal mode skips fast queries
    - Test log format includes required fields
    - _Requirements: 2.2, 2.3, 2.4, 2.5, 3.2, 3.3, 3.4, 3.5_

- [ ] 3. Implement optional Prometheus metrics
  - [ ] 3.1 Create MetricsExporter struct in src/db/instrumented.rs
    - Define `MetricsExporter` with histogram for query durations
    - Create `db_query_duration_seconds` histogram metric
    - Configure histogram buckets for database latencies (0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 5.0)
    - Implement `record_query()` method accepting query_name and duration
    - Add `query_name` label to histogram
    - _Requirements: 4.1, 4.2, 4.3, 4.5_

  - [ ] 3.2 Integrate metrics recording into InstrumentedPool
    - Add conditional metrics recording based on `enable_db_metrics` flag
    - Convert duration from milliseconds to seconds for metrics
    - Skip metrics recording when disabled to avoid overhead
    - _Requirements: 4.3, 4.4_

  - [ ]* 3.3 Write unit tests for metrics recording
    - Test metrics are recorded when enabled
    - Test metrics are skipped when disabled
    - Test histogram labels include query_name
    - Test duration conversion to seconds
    - _Requirements: 4.1, 4.2, 4.3, 4.4_

- [ ] 4. Checkpoint - Ensure all tests pass
  - Ensure all tests pass, ask the user if questions arise.

- [ ] 5. Implement timed_query helper function
  - [ ] 5.1 Create timed_query helper in src/db/instrumented.rs
    - Accept `query_name: &str` parameter
    - Accept `pool: &InstrumentedPool` parameter
    - Accept `query: sqlx::Query` parameter
    - Return `Result` compatible with sqlx::query
    - Measure execution time using Instant::now()
    - Apply logging based on configuration
    - Apply metrics recording if enabled
    - Preserve sqlx error types in return value
    - _Requirements: 6.1, 6.2, 6.3, 6.4, 6.5, 1.1, 1.2, 1.3_

  - [ ]* 5.2 Write unit tests for timed_query helper
    - Test successful query execution and timing
    - Test error propagation from sqlx
    - Test return type compatibility
    - Test logging is triggered appropriately
    - _Requirements: 6.4, 6.5_

- [ ] 6. Update database module initialization
  - [ ] 6.1 Modify src/db/mod.rs to create InstrumentedPool
    - Import InstrumentedPool from instrumented module
    - Wrap existing PgPool creation with InstrumentedPool::new()
    - Pass configuration values to InstrumentedPool
    - Initialize MetricsExporter if metrics are enabled
    - Export InstrumentedPool for use in query functions
    - _Requirements: 5.1, 5.2, 5.3_

  - [ ]* 6.2 Write integration tests for pool initialization
    - Test pool creation with various configurations
    - Test metrics exporter initialization
    - Test pool can execute queries successfully
    - _Requirements: 5.1, 5.2, 5.3, 5.5_

- [ ] 7. Retrofit existing query functions
  - [ ] 7.1 Update query functions in src/db/queries.rs to use instrumentation
    - Replace direct sqlx::query calls with timed_query helper
    - Use function name as query_identifier for each function
    - Preserve original return types
    - Preserve original error handling
    - Maintain backward compatibility with function signatures
    - Update all query functions: get_payment, create_payment, update_payment_status, etc.
    - _Requirements: 7.1, 7.2, 7.3, 7.4, 7.5_

  - [ ]* 7.2 Write integration tests for retrofitted query functions
    - Test each query function executes successfully
    - Test timing is recorded for each function
    - Test slow queries are logged
    - Test return values match original behavior
    - Test error handling matches original behavior
    - _Requirements: 7.3, 7.4, 7.5_

- [ ] 8. Add property-based tests using proptest
  - [ ]* 8.1 Write property test for timing overhead
    - Generate random query execution scenarios
    - Verify instrumentation overhead is always < 1ms
    - Test with various query durations
    - _Requirements: 1.5_

  - [ ]* 8.2 Write property test for configuration validation
    - Generate random configuration values
    - Verify invalid thresholds use defaults
    - Verify boolean parsing handles various inputs
    - _Requirements: 8.3, 8.4, 8.5_

  - [ ]* 8.3 Write property test for logging behavior
    - Generate random query durations
    - Verify slow queries are always logged when exceeding threshold
    - Verify fast queries are not logged in normal mode
    - Verify all queries are logged in debug mode
    - _Requirements: 2.2, 3.2, 3.3, 3.4, 3.5_

- [ ] 9. Final checkpoint - Ensure all tests pass
  - Ensure all tests pass, ask the user if questions arise.

## Notes

- Tasks marked with `*` are optional and can be skipped for faster MVP
- Each task references specific requirements for traceability
- Checkpoints ensure incremental validation
- Property tests validate universal correctness properties using proptest
- Unit tests validate specific examples and edge cases
- The implementation maintains backward compatibility with existing query code
- Metrics integration is optional and can be disabled for zero overhead
