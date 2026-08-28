//! Slow Query Logger & EXPLAIN ANALYZE Plan Capture Engine.
//!
//! Automatically records slow queries exceeding a latency threshold, capturing
//! their PostgreSQL `EXPLAIN ANALYZE` execution plans with write safety guards
//! and bounded memory retention.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};
use uuid::Uuid;

/// Configuration for slow query logging and EXPLAIN plan capture.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlowQueryConfig {
    /// Latency threshold in milliseconds (default: 100ms)
    pub threshold_ms: u64,
    /// Maximum number of slow query records to store in memory (default: 100)
    pub max_stored_queries: usize,
    /// Maximum plan size in bytes before truncation (default: 65,536 bytes / 64 KB)
    pub max_plan_bytes: usize,
    /// Whether PostgreSQL auto_explain extension integration is active
    pub auto_explain_enabled: bool,
    /// Whether application-level EXPLAIN plan capture is enabled
    pub capture_explain: bool,
}

impl Default for SlowQueryConfig {
    fn default() -> Self {
        Self {
            threshold_ms: 100,
            max_stored_queries: 100,
            max_plan_bytes: 65_536,
            auto_explain_enabled: true,
            capture_explain: true,
        }
    }
}

impl SlowQueryConfig {
    pub fn with_threshold(threshold_ms: u64) -> Self {
        Self {
            threshold_ms,
            ..Default::default()
        }
    }
}

/// Record representing a slow database query execution and its EXPLAIN plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlowQueryRecord {
    pub id: Uuid,
    pub query_text: String,
    pub duration_ms: u64,
    pub threshold_ms: u64,
    pub explain_plan: Option<String>,
    pub is_write_query: bool,
    pub timestamp: DateTime<Utc>,
}

/// Thread-safe logger and storage manager for slow query execution plans.
#[derive(Clone)]
pub struct SlowQueryLogger {
    config: SlowQueryConfig,
    records: Arc<RwLock<VecDeque<SlowQueryRecord>>>,
}

impl SlowQueryLogger {
    pub fn new(config: SlowQueryConfig) -> Self {
        Self {
            records: Arc::new(RwLock::new(VecDeque::with_capacity(
                config.max_stored_queries,
            ))),
            config,
        }
    }

    pub fn config(&self) -> &SlowQueryConfig {
        &self.config
    }

    /// Check if a query text is a write operation (INSERT, UPDATE, DELETE, ALTER, DROP, CREATE).
    pub fn is_write_query(query: &str) -> bool {
        let trimmed = query.trim().to_uppercase();
        trimmed.starts_with("INSERT")
            || trimmed.starts_with("UPDATE")
            || trimmed.starts_with("DELETE")
            || trimmed.starts_with("CREATE")
            || trimmed.starts_with("DROP")
            || trimmed.starts_with("ALTER")
            || trimmed.starts_with("TRUNCATE")
    }

    /// Record a query execution, automatically capturing EXPLAIN ANALYZE if duration >= threshold_ms.
    pub async fn record_query(
        &self,
        pool: Option<&PgPool>,
        query_text: &str,
        duration_ms: u64,
    ) -> Option<SlowQueryRecord> {
        if duration_ms < self.config.threshold_ms {
            return None;
        }

        let is_write = Self::is_write_query(query_text);
        warn!(
            duration_ms = duration_ms,
            threshold_ms = self.config.threshold_ms,
            is_write = is_write,
            "Slow query detected: {}",
            query_text
        );

        let mut explain_plan = None;

        // Safely capture EXPLAIN plan if pool is available and query is a read query (or auto_explain is configured)
        if self.config.capture_explain && pool.is_some() {
            if is_write {
                explain_plan = Some(
                    "[Write query: EXPLAIN ANALYZE re-execution bypassed for safety. auto_explain logs available in PostgreSQL server logs]".to_string(),
                );
            } else if let Some(pg_pool) = pool {
                explain_plan = self.capture_explain_plan(pg_pool, query_text).await;
            }
        }

        // Apply payload truncation if plan exceeds max_plan_bytes
        if let Some(ref mut plan) = explain_plan {
            if plan.len() > self.config.max_plan_bytes {
                plan.truncate(self.config.max_plan_bytes);
                plan.push_str("\n...[EXPLAIN plan truncated at max_plan_bytes limit]");
            }
        }

        let record = SlowQueryRecord {
            id: Uuid::new_v4(),
            query_text: query_text.to_string(),
            duration_ms,
            threshold_ms: self.config.threshold_ms,
            explain_plan,
            is_write_query: is_write,
            timestamp: Utc::now(),
        };

        // Bounded retention: insert into ring buffer, evicting oldest entries when full
        let mut history = self.records.write().await;
        if history.len() >= self.config.max_stored_queries {
            history.pop_front();
        }
        history.push_back(record.clone());

        Some(record)
    }

    /// Capture PostgreSQL EXPLAIN ANALYZE plan for a query.
    pub async fn capture_explain_plan(&self, pool: &PgPool, query_text: &str) -> Option<String> {
        let explain_sql = format!("EXPLAIN ANALYZE {}", query_text);
        let rows = sqlx::query_as::<_, (String,)>(&explain_sql)
            .fetch_all(pool)
            .await;

        match rows {
            Ok(lines) => {
                let plan_text = lines
                    .into_iter()
                    .map(|(line,)| line)
                    .collect::<Vec<_>>()
                    .join("\n");
                Some(plan_text)
            }
            Err(e) => {
                warn!("Failed to capture EXPLAIN ANALYZE plan: {:?}", e);
                Some(format!("[Failed to execute EXPLAIN ANALYZE: {}]", e))
            }
        }
    }

    /// Configure PostgreSQL session settings for the `auto_explain` extension.
    pub async fn configure_auto_explain_session(&self, pool: &PgPool) -> Result<(), sqlx::Error> {
        info!(
            threshold_ms = self.config.threshold_ms,
            "Configuring PostgreSQL session auto_explain settings"
        );
        let sql = format!(
            r#"
            LOAD 'auto_explain';
            SET auto_explain.log_min_duration = '{}ms';
            SET auto_explain.log_analyze = true;
            SET auto_explain.log_format = text;
            "#,
            self.config.threshold_ms
        );

        let _ = sqlx::query(&sql).execute(pool).await;
        Ok(())
    }

    /// Retrieve all recorded slow queries.
    pub async fn get_slow_queries(&self) -> Vec<SlowQueryRecord> {
        let history = self.records.read().await;
        history.iter().cloned().collect()
    }

    /// Retrieve captured slow query records containing matching query text substring.
    pub async fn get_plans_for_query(&self, query_substring: &str) -> Vec<SlowQueryRecord> {
        let history = self.records.read().await;
        let sub = query_substring.to_lowercase();
        history
            .iter()
            .filter(|r| r.query_text.to_lowercase().contains(&sub))
            .cloned()
            .collect()
    }

    /// Clear all captured slow query records.
    pub async fn clear(&self) {
        let mut history = self.records.write().await;
        history.clear();
    }
}
