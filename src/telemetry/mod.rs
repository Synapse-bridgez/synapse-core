//! Telemetry module with input validation, reconnection logic, connection pooling, secure health checks, and metrics optimization.

pub mod connection_pool;
pub mod error_handling;
pub mod health_checks;
pub mod input_validation;
pub mod metrics_optimization;
pub mod reconnection;

pub use connection_pool::{ConnectionPool, PoolConfig, PoolError};
pub use error_handling::{ErrorAction, ErrorHandler, TelemetryError, TelemetryResult};
pub use health_checks::{HealthCheckManager, HealthCheckResult, HealthCheckConfig};
//! Telemetry module with input validation, reconnection logic, and connection pooling.
//!
//! This module provides comprehensive telemetry functionality including:
//! - Error handling with consolidated TelemetryError enum
//! - Input validation for spans, attributes, and endpoints
//! - Connection pooling with resource limits and eviction
//! - Reconnection management with exponential backoff and circuit breaker
//! - Data export with batching and buffering
//!
//! All error paths are designed to degrade gracefully without panicking.

pub mod connection_pool;
pub mod data_export;
pub mod error_handling;
pub mod input_validation;
pub mod reconnection;

pub use connection_pool::ConnectionPool;
pub use data_export::{DataExportService, ExportBatch, ExportConfig, TelemetryRecord};
pub use error_handling::{ErrorAction, ErrorHandler, TelemetryError, TelemetryResult};
pub use input_validation::InputValidator;
pub use metrics_optimization::{MetricsInstruments, CardinalityLimiter};
pub use reconnection::ReconnectionManager;
