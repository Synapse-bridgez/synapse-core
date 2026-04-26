use serde_json::json;
use sqlx::PgPool;

pub struct PoolMetrics {
    pub active_connections: u32,
    pub idle_connections: u32,
    pub max_connections: u32,
    pub utilization_percent: f64,
}

impl PoolMetrics {
    pub fn from_pool(pool: &PgPool) -> Self {
        let num_connections = pool.num_connections();
        let idle_connections = pool.num_idle_connections();
        let max_connections = 5; // From db::create_pool
        let active_connections = num_connections.saturating_sub(idle_connections);
        let utilization_percent = if max_connections > 0 {
            (active_connections as f64 / max_connections as f64) * 100.0
        } else {
            0.0
        };

        Self {
            active_connections,
            idle_connections,
            max_connections,
            utilization_percent,
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "db_pool_active_connections": self.active_connections,
            "db_pool_idle_connections": self.idle_connections,
            "db_pool_max_connections": self.max_connections,
            "db_pool_utilization_percent": self.utilization_percent,
        })
    }
}

pub fn emit_metrics(metrics: &PoolMetrics) {
    tracing::info!(
        metrics = ?metrics.to_json(),
        "pool_metrics"
    );
}
