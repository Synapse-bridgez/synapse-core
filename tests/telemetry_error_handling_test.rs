//! Comprehensive tests for telemetry error handling
//!
//! Tests cover:
//! - Error type creation and conversion
//! - Error handler behavior with different configurations
//! - Circuit breaker integration
//! - Validation error handling
//! - Threshold-based error handling
//! - Error recovery scenarios

use synapse_core::telemetry::{ErrorAction, ErrorHandler, TelemetryError, TelemetryResult};

#[test]
fn test_telemetry_error_types() {
    let init_error = TelemetryError::InitializationError("failed to init".to_string());
    assert!(init_error.to_string().contains("initialize tracer"));

    let exporter_error = TelemetryError::ExporterConfigError("bad config".to_string());
    assert!(exporter_error.to_string().contains("configure OTLP"));

    let export_error = TelemetryError::ExportError("export failed".to_string());
    assert!(export_error.to_string().contains("export spans"));

    let shutdown_error = TelemetryError::ShutdownError("shutdown failed".to_string());
    assert!(shutdown_error.to_string().contains("shutdown tracer"));

    let endpoint_error = TelemetryError::InvalidEndpoint("bad endpoint".to_string());
    assert!(endpoint_error.to_string().contains("Invalid endpoint"));

    let connection_error = TelemetryError::ConnectionError("connection lost".to_string());
    assert!(connection_error.to_string().contains("Connection error"));

    let circuit_breaker_error = TelemetryError::CircuitBreakerOpen;
    assert!(circuit_breaker_error
        .to_string()
        .contains("Circuit breaker"));

    let timeout_error = TelemetryError::Timeout(std::time::Duration::from_secs(30));
    assert!(timeout_error.to_string().contains("timed out"));
}

#[test]
fn test_error_handler_default_behavior() {
    let handler = ErrorHandler::new();
    assert_eq!(handler.error_count(), 0);
    assert!(!handler.threshold_exceeded());
}

#[test]
fn test_error_handler_fail_fast_mode() {
    let mut handler = ErrorHandler::fail_fast();

    let error = TelemetryError::ExportError("test error".to_string());
    let action = handler.handle_error(&error);

    assert_eq!(action, ErrorAction::Stop);
    assert_eq!(handler.error_count(), 1);
}

#[test]
fn test_error_handler_threshold_behavior() {
    let mut handler = ErrorHandler::with_threshold(3);
    let error = TelemetryError::ConnectionError("connection failed".to_string());

    // First error - should continue
    let action1 = handler.handle_error(&error);
    assert_eq!(action1, ErrorAction::Continue);
    assert_eq!(handler.error_count(), 1);
    assert!(!handler.threshold_exceeded());

    // Second error - should continue
    let action2 = handler.handle_error(&error);
    assert_eq!(action2, ErrorAction::Continue);
    assert_eq!(handler.error_count(), 2);
    assert!(!handler.threshold_exceeded());

    // Third error - should stop (threshold reached)
    let action3 = handler.handle_error(&error);
    assert_eq!(action3, ErrorAction::Stop);
    assert_eq!(handler.error_count(), 3);
    assert!(handler.threshold_exceeded());
}

#[test]
fn test_error_handler_reset() {
    let mut handler = ErrorHandler::with_threshold(5);
    let error = TelemetryError::ExportError("test".to_string());

    // Generate some errors
    handler.handle_error(&error);
    handler.handle_error(&error);
    handler.handle_error(&error);
    assert_eq!(handler.error_count(), 3);

    // Reset should clear error count
    handler.reset();
    assert_eq!(handler.error_count(), 0);
    assert!(!handler.threshold_exceeded());

    // Should be able to handle more errors after reset
    let action = handler.handle_error(&error);
    assert_eq!(action, ErrorAction::Continue);
    assert_eq!(handler.error_count(), 1);
}

#[test]
fn test_circuit_breaker_error_always_stops() {
    let mut handler = ErrorHandler::new();
    let error = TelemetryError::CircuitBreakerOpen;

    let action = handler.handle_error(&error);
    assert_eq!(action, ErrorAction::Stop);
}

#[test]
fn test_validation_error_fails_fast() {
    use synapse_core::telemetry::input_validation::ValidationError;

    let mut handler = ErrorHandler::new();
    let validation_error = ValidationError::EmptyValue("span name empty".to_string());
    let error = TelemetryError::ValidationError(validation_error);

    let action = handler.handle_error(&error);
    assert_eq!(action, ErrorAction::FailFast);
}

#[test]
fn test_multiple_error_types_in_sequence() {
    let mut handler = ErrorHandler::with_threshold(5);

    // Mix different error types
    let errors = [
        TelemetryError::ConnectionError("conn1".to_string()),
        TelemetryError::ExportError("export1".to_string()),
        TelemetryError::ConnectionError("conn2".to_string()),
        TelemetryError::ExportError("export2".to_string()),
    ];

    for (i, error) in errors.iter().enumerate() {
        let action = handler.handle_error(error);
        assert_eq!(action, ErrorAction::Continue);
        assert_eq!(handler.error_count(), i + 1);
    }

    // One more should still continue (threshold is 5)
    let action = handler.handle_error(&TelemetryError::ExportError("export3".to_string()));
    assert_eq!(action, ErrorAction::Stop);
    assert!(handler.threshold_exceeded());
}

#[test]
fn test_error_action_display() {
    assert_eq!(ErrorAction::Continue.to_string(), "continue");
    assert_eq!(ErrorAction::Stop.to_string(), "stop");
    assert_eq!(ErrorAction::FailFast.to_string(), "fail_fast");
}

#[test]
fn test_telemetry_result_type() {
    // Test success case
    let success: TelemetryResult<i32> = Ok(42);
    assert!(success.is_ok());
    assert_eq!(success.ok(), Some(42));

    // Test error case
    let error: TelemetryResult<i32> = Err(TelemetryError::ExportError("failed".to_string()));
    assert!(error.is_err());
}

#[test]
fn test_error_handler_with_zero_threshold() {
    let mut handler = ErrorHandler::with_threshold(0);
    let error = TelemetryError::ExportError("test".to_string());

    // With threshold 0, should stop immediately
    let action = handler.handle_error(&error);
    assert_eq!(action, ErrorAction::Stop);
}

#[test]
fn test_error_handler_with_large_threshold() {
    let mut handler = ErrorHandler::with_threshold(1000);
    let error = TelemetryError::ConnectionError("test".to_string());

    // Should continue for many errors
    for i in 0..999 {
        let action = handler.handle_error(&error);
        assert_eq!(action, ErrorAction::Continue);
        assert_eq!(handler.error_count(), i + 1);
    }

    // 1000th error should stop
    let action = handler.handle_error(&error);
    assert_eq!(action, ErrorAction::Stop);
    assert_eq!(handler.error_count(), 1000);
}

#[test]
fn test_timeout_error_with_different_durations() {
    let timeout_1s = TelemetryError::Timeout(std::time::Duration::from_secs(1));
    let timeout_30s = TelemetryError::Timeout(std::time::Duration::from_secs(30));
    let timeout_1m = TelemetryError::Timeout(std::time::Duration::from_secs(60));

    assert!(timeout_1s.to_string().contains("1s"));
    assert!(timeout_30s.to_string().contains("30s"));
    assert!(timeout_1m.to_string().contains("60s"));
}

#[test]
fn test_error_recovery_scenario() {
    let mut handler = ErrorHandler::with_threshold(3);
    let error = TelemetryError::ConnectionError("intermittent".to_string());

    // Simulate intermittent failures
    handler.handle_error(&error);
    handler.handle_error(&error);
    assert_eq!(handler.error_count(), 2);

    // Successful operation - reset
    handler.reset();
    assert_eq!(handler.error_count(), 0);

    // More failures after recovery
    handler.handle_error(&error);
    assert_eq!(handler.error_count(), 1);
    assert!(!handler.threshold_exceeded());
}

#[test]
fn test_validation_error_conversion() {
    use synapse_core::telemetry::input_validation::ValidationError;

    let validation_errors = vec![
        ValidationError::EmptyValue("empty".to_string()),
        ValidationError::TooLong("too long".to_string()),
        ValidationError::InvalidFormat("invalid".to_string()),
        ValidationError::TooMany("too many".to_string()),
    ];

    for val_err in validation_errors {
        let tel_err: TelemetryError = val_err.into();
        assert!(matches!(tel_err, TelemetryError::ValidationError(_)));
    }
}

#[test]
fn test_concurrent_error_handling() {
    use std::sync::{Arc, Mutex};
    use std::thread;

    let handler = Arc::new(Mutex::new(ErrorHandler::with_threshold(100)));
    let mut handles = vec![];

    // Simulate concurrent error handling from multiple threads
    for _ in 0..10 {
        let handler_clone = Arc::clone(&handler);
        let handle = thread::spawn(move || {
            let error = TelemetryError::ExportError("concurrent".to_string());
            for _ in 0..5 {
                let mut h = handler_clone.lock().unwrap();
                h.handle_error(&error);
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    let final_handler = handler.lock().unwrap();
    assert_eq!(final_handler.error_count(), 50); // 10 threads * 5 errors each
}

#[test]
fn test_error_handler_clone() {
    let handler1 = ErrorHandler::with_threshold(5);
    let handler2 = handler1.clone();

    assert_eq!(handler1.error_count(), handler2.error_count());
}

#[test]
fn test_error_debug_format() {
    let error = TelemetryError::InitializationError("debug test".to_string());
    let debug_str = format!("{:?}", error);
    assert!(debug_str.contains("InitializationError"));
    assert!(debug_str.contains("debug test"));
}
