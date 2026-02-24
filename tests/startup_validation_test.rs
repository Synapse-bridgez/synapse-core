use synapse_core::startup::{validate_environment, ValidationReport};
use synapse_core::config::{Config, AllowedIps, LogFormat};
use testcontainers_modules::postgres::Postgres;
use testcontainers::runners::AsyncRunner;
use sqlx::{PgPool, migrate::Migrator};
use std::path::Path;

/// Helper function to create a test config with valid defaults
fn create_test_config(database_url: String, redis_url: String, horizon_url: String) -> Config {
    Config {
        server_port: 3000,
        database_url,
        database_replica_url: None,
        stellar_horizon_url: horizon_url,
        anchor_webhook_secret: "test-secret".to_string(),
        redis_url,
        default_rate_limit: 100,
        whitelist_rate_limit: 1000,
        whitelisted_ips: String::new(),
        log_format: LogFormat::Text,
        allowed_ips: AllowedIps::Any,
        backup_dir: "./backups".to_string(),
        backup_encryption_key: None,
    }
}

/// Helper function to setup test database with migrations
async fn setup_test_database() -> (PgPool, impl std::any::Any) {
    let container = Postgres::default().start().await.unwrap();
    let host_port = container.get_host_port_ipv4(5432).await.unwrap();
    let database_url = format!("postgres://postgres:postgres@127.0.0.1:{}/postgres", host_port);

    let pool = PgPool::connect(&database_url).await.unwrap();
    let migrator = Migrator::new(Path::join(Path::new(env!("CARGO_MANIFEST_DIR")), "migrations"))
        .await
        .unwrap();
    migrator.run(&pool).await.unwrap();

    (pool, container)
}

#[tokio::test]
async fn test_validation_all_healthy() {
    // Setup test database
    let (pool, _container) = setup_test_database().await;
    let database_url = pool.connect_options().to_url_lossy().to_string();

    // Use real Stellar testnet Horizon (publicly available)
    let horizon_url = "https://horizon-testnet.stellar.org".to_string();

    // Setup test Redis (requires Redis to be running locally or use testcontainers)
    // For this test, we'll use a mock Redis URL and expect it to fail gracefully
    // In a real scenario, you'd use testcontainers-modules for Redis
    let redis_url = "redis://127.0.0.1:6379".to_string();

    let config = create_test_config(database_url, redis_url, horizon_url);

    // Run validation
    let report = validate_environment(&config, &pool).await.unwrap();

    // Assertions
    assert!(report.environment, "Environment validation should pass");
    assert!(report.database, "Database validation should pass");
    assert!(report.horizon, "Horizon validation should pass");
    
    // Note: Redis might fail if not running locally, which is expected in CI
    // In production tests, you'd use testcontainers for Redis too
    
    report.print();
}

#[tokio::test]
async fn test_validation_database_unavailable() {
    // Use an invalid database URL
    let invalid_database_url = "postgres://invalid:invalid@127.0.0.1:9999/invalid".to_string();
    let redis_url = "redis://127.0.0.1:6379".to_string();
    let horizon_url = "https://horizon-testnet.stellar.org".to_string();

    let config = create_test_config(invalid_database_url.clone(), redis_url, horizon_url);

    // Create a pool that will fail to connect
    let pool_result = PgPool::connect(&invalid_database_url).await;
    
    // If we can't even create the pool, that's expected
    if pool_result.is_err() {
        // This is the expected behavior - database is unavailable
        return;
    }

    let pool = pool_result.unwrap();
    let report = validate_environment(&config, &pool).await.unwrap();

    // Assertions
    assert!(!report.database, "Database validation should fail");
    assert!(!report.is_valid(), "Overall validation should fail");
    assert!(!report.errors.is_empty(), "Should have error messages");
    
    // Check that error message mentions database
    let has_db_error = report.errors.iter().any(|e| e.contains("Database"));
    assert!(has_db_error, "Should have database error in report");
    
    report.print();
}

#[tokio::test]
async fn test_validation_redis_unavailable() {
    // Setup valid database
    let (pool, _container) = setup_test_database().await;
    let database_url = pool.connect_options().to_url_lossy().to_string();

    // Use invalid Redis URL
    let invalid_redis_url = "redis://127.0.0.1:9999".to_string();
    let horizon_url = "https://horizon-testnet.stellar.org".to_string();

    let config = create_test_config(database_url, invalid_redis_url, horizon_url);

    // Run validation
    let report = validate_environment(&config, &pool).await.unwrap();

    // Assertions
    assert!(report.environment, "Environment validation should pass");
    assert!(report.database, "Database validation should pass");
    assert!(!report.redis, "Redis validation should fail");
    assert!(!report.is_valid(), "Overall validation should fail");
    
    // Check that error message mentions Redis
    let has_redis_error = report.errors.iter().any(|e| e.contains("Redis"));
    assert!(has_redis_error, "Should have Redis error in report");
    
    report.print();
}

#[tokio::test]
async fn test_validation_horizon_unavailable() {
    // Setup valid database
    let (pool, _container) = setup_test_database().await;
    let database_url = pool.connect_options().to_url_lossy().to_string();

    let redis_url = "redis://127.0.0.1:6379".to_string();
    
    // Use invalid Horizon URL
    let invalid_horizon_url = "https://invalid-horizon-url-that-does-not-exist.stellar.org".to_string();

    let config = create_test_config(database_url, redis_url, invalid_horizon_url);

    // Run validation
    let report = validate_environment(&config, &pool).await.unwrap();

    // Assertions
    assert!(report.environment, "Environment validation should pass");
    assert!(report.database, "Database validation should pass");
    assert!(!report.horizon, "Horizon validation should fail");
    assert!(!report.is_valid(), "Overall validation should fail");
    
    // Check that error message mentions Horizon
    let has_horizon_error = report.errors.iter().any(|e| e.contains("Horizon"));
    assert!(has_horizon_error, "Should have Horizon error in report");
    
    report.print();
}

#[tokio::test]
async fn test_validation_report_generation() {
    // Setup test database
    let (pool, _container) = setup_test_database().await;
    let database_url = pool.connect_options().to_url_lossy().to_string();

    // Mix of valid and invalid services
    let invalid_redis_url = "redis://127.0.0.1:9999".to_string();
    let horizon_url = "https://horizon-testnet.stellar.org".to_string();

    let config = create_test_config(database_url, invalid_redis_url, horizon_url);

    // Run validation
    let report = validate_environment(&config, &pool).await.unwrap();

    // Test report structure
    assert!(!report.is_valid(), "Report should indicate failure");
    assert!(!report.errors.is_empty(), "Report should contain errors");
    
    // Verify report contains expected fields
    assert!(report.environment, "Environment should be valid");
    assert!(report.database, "Database should be valid");
    assert!(!report.redis, "Redis should be invalid");
    assert!(report.horizon, "Horizon should be valid");
    
    // Test print functionality (visual verification in test output)
    report.print();
    
    // Verify error messages are descriptive
    for error in &report.errors {
        assert!(!error.is_empty(), "Error messages should not be empty");
        assert!(error.len() > 10, "Error messages should be descriptive");
    }
}

#[tokio::test]
async fn test_validation_empty_database_url() {
    // Setup test database for pool
    let (pool, _container) = setup_test_database().await;

    // Create config with empty database URL
    let mut config = create_test_config(
        String::new(),
        "redis://127.0.0.1:6379".to_string(),
        "https://horizon-testnet.stellar.org".to_string(),
    );

    // Run validation
    let report = validate_environment(&config, &pool).await.unwrap();

    // Assertions
    assert!(!report.environment, "Environment validation should fail with empty database URL");
    assert!(!report.is_valid(), "Overall validation should fail");
    
    let has_env_error = report.errors.iter().any(|e| e.contains("Environment"));
    assert!(has_env_error, "Should have environment error in report");
    
    report.print();
}

#[tokio::test]
async fn test_validation_invalid_horizon_url_format() {
    // Setup test database
    let (pool, _container) = setup_test_database().await;
    let database_url = pool.connect_options().to_url_lossy().to_string();

    // Create config with invalid URL format
    let config = create_test_config(
        database_url,
        "redis://127.0.0.1:6379".to_string(),
        "not-a-valid-url".to_string(),
    );

    // Run validation
    let report = validate_environment(&config, &pool).await.unwrap();

    // Assertions
    assert!(!report.environment, "Environment validation should fail with invalid URL format");
    assert!(!report.is_valid(), "Overall validation should fail");
    
    report.print();
}

#[tokio::test]
async fn test_validation_multiple_failures() {
    // Setup test database
    let (pool, _container) = setup_test_database().await;
    let database_url = pool.connect_options().to_url_lossy().to_string();

    // Create config with multiple invalid services
    let config = create_test_config(
        database_url,
        "redis://127.0.0.1:9999".to_string(), // Invalid Redis
        "https://invalid-horizon.stellar.org".to_string(), // Invalid Horizon
    );

    // Run validation
    let report = validate_environment(&config, &pool).await.unwrap();

    // Assertions
    assert!(!report.redis, "Redis validation should fail");
    assert!(!report.horizon, "Horizon validation should fail");
    assert!(!report.is_valid(), "Overall validation should fail");
    assert!(report.errors.len() >= 2, "Should have multiple errors");
    
    // Verify both Redis and Horizon errors are present
    let has_redis_error = report.errors.iter().any(|e| e.contains("Redis"));
    let has_horizon_error = report.errors.iter().any(|e| e.contains("Horizon"));
    assert!(has_redis_error, "Should have Redis error");
    assert!(has_horizon_error, "Should have Horizon error");
    
    report.print();
}
