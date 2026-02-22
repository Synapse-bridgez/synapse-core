// Metrics module for Prometheus metrics collection and exposition

use metrics_exporter_prometheus::PrometheusHandle;
use std::time::Duration;

/// Status of a callback for metrics labeling
#[derive(Debug, Clone, Copy)]
pub enum CallbackStatus {
    Success,
    Error,
    Invalid,
}

impl CallbackStatus {
    fn as_str(&self) -> &'static str {
        match self {
            CallbackStatus::Success => "success",
            CallbackStatus::Error => "error",
            CallbackStatus::Invalid => "invalid",
        }
    }
}

/// Initialize the Prometheus metrics exporter and register all metrics
/// Returns a PrometheusHandle for rendering metrics
pub fn init_metrics() -> Result<PrometheusHandle, Box<dyn std::error::Error>> {
    let builder = metrics_exporter_prometheus::PrometheusBuilder::new();
    
    // Configure histogram buckets for transaction processing duration
    let builder = builder.set_buckets_for_metric(
        metrics_exporter_prometheus::Matcher::Full("transaction_processing_seconds".to_string()),
        &[0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 5.0, 10.0],
    )?;
    
    let handle = builder.install_recorder()?;
    
    // Register metrics with descriptors
    metrics::describe_counter!(
        "callbacks_received_total",
        "Total number of callbacks received by status"
    );
    
    metrics::describe_histogram!(
        "transaction_processing_seconds",
        metrics::Unit::Seconds,
        "Transaction processing duration in seconds"
    );
    
    metrics::describe_gauge!(
        "active_db_connections",
        "Current number of active database connections"
    );
    
    tracing::info!("Metrics registry initialized successfully");
    Ok(handle)
}

/// Record a callback reception event with the given status
pub fn record_callback(status: CallbackStatus) {
    metrics::counter!("callbacks_received_total", "status" => status.as_str()).increment(1);
}

/// Record transaction processing duration
pub fn record_transaction_duration(duration: Duration) {
    let seconds = duration.as_secs_f64();
    metrics::histogram!("transaction_processing_seconds").record(seconds);
}

/// Update the active database connections gauge
pub fn update_db_connections(count: usize) {
    metrics::gauge!("active_db_connections").set(count as f64);
}

/// Handler for the /metrics endpoint
/// Returns metrics in Prometheus text exposition format
pub async fn metrics_handler(
    metrics_handle: axum::extract::State<PrometheusHandle>,
    pool: axum::extract::State<sqlx::PgPool>,
) -> impl axum::response::IntoResponse {
    use axum::http::{header, StatusCode};
    
    // Update database connections gauge before rendering
    let pool_size = pool.size() as usize;
    update_db_connections(pool_size);
    
    let metrics = metrics_handle.render();
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
        metrics
    )
}

/// Middleware to restrict /metrics endpoint to allowed IPs
pub async fn metrics_auth_middleware(
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    axum::extract::State(config): axum::extract::State<crate::config::Config>,
    request: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, axum::http::StatusCode> {
    let client_ip = addr.ip();
    
    // Allow if:
    // - Metrics are disabled (shouldn't reach here, but defensive)
    // - IP whitelist is empty (allow all)
    // - IP is in whitelist
    // - IP is localhost
    if !config.metrics_enabled {
        tracing::warn!("Metrics access attempt from {} but metrics are disabled", client_ip);
        return Err(axum::http::StatusCode::FORBIDDEN);
    }
    
    if config.metrics_allowed_ips.is_empty() 
        || config.metrics_allowed_ips.contains(&client_ip) 
        || client_ip.is_loopback() {
        tracing::debug!("Metrics access granted to {}", client_ip);
        Ok(next.run(request).await)
    } else {
        tracing::warn!("Unauthorized metrics access attempt from {}", client_ip);
        Err(axum::http::StatusCode::FORBIDDEN)
    }
}
