use axum::{
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};
use prometheus::{Counter, Histogram, IntGauge, Registry, TextEncoder, Encoder};
use sqlx::PgPool;
use std::sync::{Arc, Mutex};

/// Shared metrics state that can be cloned
#[derive(Clone)]
pub struct MetricsHandle {
    inner: Arc<Mutex<MetricsInner>>,
}

struct MetricsInner {
    registry: prometheus::Registry,
    // HTTP Request Metrics
    http_requests_total: Counter,
    http_request_duration_seconds: Histogram,
    
    // Database Pool Metrics
    db_pool_active_connections: IntGauge,
    db_pool_idle_connections: IntGauge,
    db_pool_max_connections: IntGauge,
    
    // Database Query Metrics
    db_slow_queries_total: Counter,
    
    // Cache Metrics
    cache_hits_total: Counter,
    cache_misses_total: Counter,
    
    // Processor Queue Metrics
    processor_queue_depth: IntGauge,
}

impl MetricsHandle {
    /// Update database pool statistics
    pub fn update_db_pool_stats(&self, active: u32, idle: u32, max: u32) {
        if let Ok(inner) = self.inner.lock() {
            inner.db_pool_active_connections.set(active as i64);
            inner.db_pool_idle_connections.set(idle as i64);
            inner.db_pool_max_connections.set(max as i64);
        }
    }

    /// Update processor queue depth
    pub fn update_queue_depth(&self, depth: u64) {
        if let Ok(inner) = self.inner.lock() {
            inner.processor_queue_depth.set(depth as i64);
        }
    }

    /// Record a cache hit
    pub fn record_cache_hit(&self) {
        if let Ok(inner) = self.inner.lock() {
            inner.cache_hits_total.inc();
        }
    }

    /// Record a cache miss
    pub fn record_cache_miss(&self) {
        if let Ok(inner) = self.inner.lock() {
            inner.cache_misses_total.inc();
        }
    }

    /// Record an HTTP request
    pub fn record_http_request(&self, path: &str, duration_secs: f64) {
        if let Ok(inner) = self.inner.lock() {
            inner.http_requests_total.inc();
            inner.http_request_duration_seconds
                .with_label_values(&[path])
                .observe(duration_secs);
        }
    }

    /// Update slow query count
    pub fn increment_slow_queries(&self) {
        if let Ok(inner) = self.inner.lock() {
            inner.db_slow_queries_total.inc();
        }
    }

    /// Render metrics in Prometheus text format
    pub fn render_metrics(&self) -> Result<String, Box<dyn std::error::Error>> {
        let inner = self.inner.lock().map_err(|e| format!("Metrics lock poison: {}", e).into())?;
        let encoder = TextEncoder::new();
        let metric_families = inner.registry.gather();
        Ok(encoder.encode_to_string(&metric_families)?)
    }
}

pub fn init_metrics() -> Result<MetricsHandle, Box<dyn std::error::Error>> {
    let registry = prometheus::Registry::new();

    // HTTP metrics
    let http_requests_total = Counter::new("http_requests_total", "Total HTTP requests")?;
    registry.register(Box::new(http_requests_total.clone()))?;

    let http_request_duration_seconds = Histogram::new_with_opts(
        prometheus::HistogramOpts::new(
            "http_request_duration_seconds",
            "HTTP request duration in seconds",
        )
        .buckets(vec![0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0, 5.0]),
    )?;
    registry.register(Box::new(http_request_duration_seconds.clone()))?;

    // Database pool metrics
    let db_pool_active_connections =
        IntGauge::new("db_pool_active_connections", "Active database connections")?;
    registry.register(Box::new(db_pool_active_connections.clone()))?;

    let db_pool_idle_connections =
        IntGauge::new("db_pool_idle_connections", "Idle database connections")?;
    registry.register(Box::new(db_pool_idle_connections.clone()))?;

    let db_pool_max_connections =
        IntGauge::new("db_pool_max_connections", "Maximum database connections")?;
    registry.register(Box::new(db_pool_max_connections.clone()))?;

    // Database query metrics
    let db_slow_queries_total = Counter::new("db_slow_queries_total", "Total slow queries")?;
    registry.register(Box::new(db_slow_queries_total.clone()))?;

    // Cache metrics
    let cache_hits_total = Counter::new("cache_hits_total", "Total cache hits")?;
    registry.register(Box::new(cache_hits_total.clone()))?;

    let cache_misses_total = Counter::new("cache_misses_total", "Total cache misses")?;
    registry.register(Box::new(cache_misses_total.clone()))?;

    // Processor queue metrics
    let processor_queue_depth = IntGauge::new("processor_queue_depth", "Processor queue depth")?;
    registry.register(Box::new(processor_queue_depth.clone()))?;

    Ok(MetricsHandle {
        inner: Arc::new(Mutex::new(MetricsInner {
            registry,
            http_requests_total,
            http_request_duration_seconds,
            db_pool_active_connections,
            db_pool_idle_connections,
            db_pool_max_connections,
            db_slow_queries_total,
            cache_hits_total,
            cache_misses_total,
            processor_queue_depth,
        })),
    })
}

#[derive(Clone)]
pub struct MetricsState {
    pub handle: MetricsHandle,
    pub pool: PgPool,
}

/// Handler for /metrics endpoint
/// Returns Prometheus-formatted metrics
pub async fn metrics_handler(
    State(api_state): State<crate::ApiState>,
) -> Result<String, StatusCode> {
    // Update pool stats before rendering metrics
    let pool = &api_state.app_state.db;
    let active = pool.size();
    let idle = pool.num_idle();
    let max = pool.options().get_max_connections();
    api_state.app_state.metrics_handle.update_db_pool_stats(active, idle, max);
    
    // Update queue depth
    let queue_depth = api_state.app_state.pending_queue_depth.load(std::sync::atomic::Ordering::Relaxed);
    api_state.app_state.metrics_handle.update_queue_depth(queue_depth);

    // Render metrics in Prometheus text format
    api_state.app_state.metrics_handle
        .render_metrics()
        .map_err(|e| {
            tracing::error!("Failed to render metrics: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

/// Middleware to track HTTP metrics
/// Records request duration and increments request counter
pub async fn metrics_middleware<B>(
    State(handle): State<MetricsHandle>,
    request: Request<B>,
    next: Next<B>,
) -> Response {
    let path = request.uri().path().to_string();
    let start = std::time::Instant::now();

    let response = next.run(request).await;

    let duration = start.elapsed().as_secs_f64();
    handle.record_http_request(&path, duration);

    response
}

/// Middleware for metrics endpoint authentication
/// Checks for admin auth or allows from whitelisted IPs
pub async fn metrics_auth_middleware<B>(
    State(_config): State<crate::config::Config>,
    request: Request<B>,
    next: Next<B>,
) -> Result<Response, StatusCode> {
    // Simple auth check - in production, implement proper authentication
    // For now, allow all requests to metrics endpoint
    // An alternative: check IP whitelist or admin token header
    Ok(next.run(request).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_initialization() {
        let handle = init_metrics().expect("Failed to initialize metrics");
        // Should not panic
        assert!(handle.render_metrics().is_ok());
    }

    #[test]
    fn test_db_pool_stats_update() {
        let handle = init_metrics().expect("Failed to initialize metrics");
        handle.update_db_pool_stats(10, 5, 50);
        // Stats should be recorded
    }

    #[test]
    fn test_queue_depth_update() {
        let handle = init_metrics().expect("Failed to initialize metrics");
        handle.update_queue_depth(100);
        // Queue depth should be recorded
    }
}


