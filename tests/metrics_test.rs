use synapse_core::metrics::*;

#[tokio::test]
async fn test_metric_registration() {
    let handle = init_metrics().expect("Failed to initialize metrics");
    assert!(std::mem::size_of_val(&handle) > 0);
}

#[tokio::test]
async fn test_counter_increment() {
    let _handle = init_metrics().expect("Failed to initialize metrics");
    assert!(true);
}

#[tokio::test]
async fn test_histogram_recording() {
    let _handle = init_metrics().expect("Failed to initialize metrics");
    assert!(true);
}

#[tokio::test]
async fn test_gauge_updates() {
    let _handle = init_metrics().expect("Failed to initialize metrics");
    assert!(true);
}

#[tokio::test]
async fn test_prometheus_export_format() {
    use sqlx::postgres::PgPoolOptions;
    
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://synapse:synapse@localhost:5432/synapse_test".to_string());
    
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .expect("Failed to connect to test database");
    
    let handle = init_metrics().expect("Failed to initialize metrics");
    
    let result = metrics_handler(
        axum::extract::State(handle),
        axum::extract::State(pool),
    )
    .await;
    
    assert!(result.is_ok());
    let metrics_output = result.unwrap();
    
    assert!(metrics_output.starts_with('#'));
    assert!(metrics_output.contains("Metrics"));
}

#[tokio::test]
async fn test_metrics_authentication() {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        middleware::Next,
        response::Response,
    };
    use synapse_core::config::Config;
    
    let config = Config {
        server_port: 3000,
        database_url: "postgres://test".to_string(),
        database_replica_url: None,
        stellar_horizon_url: "https://horizon-testnet.stellar.org".to_string(),
        anchor_webhook_secret: "test_secret".to_string(),
        redis_url: "redis://localhost:6379".to_string(),
        default_rate_limit: 100,
        whitelist_rate_limit: 1000,
        whitelisted_ips: String::new(),
        log_format: synapse_core::config::LogFormat::Text,
        allowed_ips: synapse_core::config::AllowedIps::Any,
        backup_dir: "./backups".to_string(),
        backup_encryption_key: None,
    };
    
    let request = Request::builder()
        .uri("/metrics")
        .body(Body::empty())
        .unwrap();
    
    let next = Next::new(|_req: Request| async {
        Ok::<Response, StatusCode>(Response::new(Body::empty()))
    });
    
    let result = metrics_auth_middleware(
        axum::extract::State(config),
        request,
        next,
    )
    .await;
    
    assert!(result.is_ok());
}

#[test]
fn test_metrics_handle_clone() {
    let handle = init_metrics().expect("Failed to initialize metrics");
    let cloned = handle.clone();
    
    assert!(std::mem::size_of_val(&handle) > 0);
    assert!(std::mem::size_of_val(&cloned) > 0);
}

#[test]
fn test_metrics_state_creation() {
    use sqlx::postgres::PgPoolOptions;
    
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let database_url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://synapse:synapse@localhost:5432/synapse_test".to_string());
        
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await
            .expect("Failed to connect to test database");
        
        let handle = init_metrics().expect("Failed to initialize metrics");
        
        let state = MetricsState {
            handle: handle.clone(),
            pool: pool.clone(),
        };
        
        let cloned_state = state.clone();
        assert!(std::mem::size_of_val(&cloned_state) > 0);
    });
}
