//! # Slow Query Logging and Plan Capture
//!
//! This module:
//! 1. Logs queries that exceed `slow_query_threshold_ms` (existing behaviour).
//! 2. **Captures an `EXPLAIN (ANALYZE, BUFFERS, FORMAT TEXT)` plan** for every
//!    slow query and persists it to the `slow_query_plans` table for post-hoc
//!    analysis.
//!
//! ## Plan Capture Strategy
//!
//! Plans are captured via an **application-level `EXPLAIN` re-execution** on a
//! *separate connection* from the pool, issued asynchronously (fire-and-forget)
//! **only for read queries**.  Write queries are never re-executed to avoid
//! side effects.
//!
//! This is consistent with the constraint stated in the issue: the plan capture
//! **must not add latency to the already-slow query**.  The original query
//! completes and returns to the caller before plan capture starts.
//!
//! ### Why not `auto_explain`?
//!
//! `auto_explain` requires superuser access to load the extension and cannot be
//! enabled per-session without a superuser `LOAD` call.  Our pool connections
//! run as a restricted application role, so `auto_explain` is not safely
//! available.  The application-level approach matches the constraint "prefer
//! `auto_explain` or session settings *if available*; fall back to
//! application-level EXPLAIN otherwise."
//!
//! ### Safety for writes
//!
//! [`QueryKind`] must be supplied by the caller.  Only `QueryKind::Read`
//! queries are re-executed with `EXPLAIN`.  Write queries get a log entry and
//! metrics but **no plan capture**, eliminating re-execution risk entirely.
//!
//! ## Storage Retention
//!
//! Call [`prune_old_plans`] periodically (e.g., from the cron scheduler) to
//! delete rows older than `retention_days`.  Default retention is 30 days.
//!
//! ## Querying Captured Plans
//!
//! ```sql
//! -- All slow queries in the last 24 hours, slowest first:
//! SELECT captured_at, query_name, duration_ms, threshold_ms, plan_text
//! FROM slow_query_plans
//! WHERE captured_at > NOW() - INTERVAL '24 hours'
//! ORDER BY duration_ms DESC;
//!
//! -- Plans for a specific query by name:
//! SELECT plan_text FROM slow_query_plans
//! WHERE query_name = 'get_daily_totals'
//! ORDER BY captured_at DESC LIMIT 10;
//! ```

use sqlx::PgPool;
use std::sync::atomic::{AtomicU64, Ordering};
use uuid::Uuid;

/// Counter for slow queries detected (includes both read and write).
pub static SLOW_QUERY_COUNT: AtomicU64 = AtomicU64::new(0);

/// Default number of days to retain captured plans.
const DEFAULT_RETENTION_DAYS: i64 = 30;

/// Maximum length of a stored plan (bytes).  Plans larger than this are
/// truncated with a notice to keep storage bounded.
const MAX_PLAN_BYTES: usize = 65_536; // 64 KiB

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Whether a query reads or writes data.
///
/// Only `Read` queries are eligible for `EXPLAIN` re-execution to avoid
/// write side effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryKind {
    Read,
    Write,
}

/// Log a database query with timing information and, for slow *read* queries,
/// asynchronously capture and persist an `EXPLAIN ANALYZE` plan.
///
/// # Arguments
/// * `pool`                  – Connection pool used for the `EXPLAIN` query (separate connection).
/// * `query_name`            – Human-readable name (e.g., `"fetch_transaction"`).
/// * `query_sql`             – Parameterised SQL text (e.g., `"SELECT * FROM t WHERE id = $1"`).
/// * `duration_ms`           – Observed execution duration in milliseconds.
/// * `rows_affected`         – Rows returned or affected.
/// * `slow_query_threshold_ms` – Threshold above which the query is "slow".
/// * `kind`                  – `Read` or `Write` — writes are never re-executed.
///
/// # Overhead
/// Plan capture is spawned on a Tokio task and does **not** block the caller.
/// The already-slow query's response time is unaffected.
pub fn log_query_timing(
    query_name: &str,
    query_sql: &str,
    duration_ms: u64,
    rows_affected: usize,
    slow_query_threshold_ms: u64,
) {
    // Preserve backward-compatible signature (no pool, no kind) — no plan
    // capture in this variant.
    log_query_timing_inner(
        None,
        query_name,
        query_sql,
        duration_ms,
        rows_affected,
        slow_query_threshold_ms,
        QueryKind::Read, // safe default: read-only explain
    );
}

/// Extended variant that accepts a pool and query kind, enabling plan capture.
///
/// Use this in call sites that have access to the pool (e.g., inside query
/// helper functions).
pub fn log_query_timing_with_plan(
    pool: &PgPool,
    query_name: &str,
    query_sql: &str,
    duration_ms: u64,
    rows_affected: usize,
    slow_query_threshold_ms: u64,
    kind: QueryKind,
) {
    log_query_timing_inner(
        Some(pool.clone()),
        query_name,
        query_sql,
        duration_ms,
        rows_affected,
        slow_query_threshold_ms,
        kind,
    );
}

// ---------------------------------------------------------------------------
// Internal implementation
// ---------------------------------------------------------------------------

fn log_query_timing_inner(
    pool: Option<PgPool>,
    query_name: &str,
    query_sql: &str,
    duration_ms: u64,
    rows_affected: usize,
    slow_query_threshold_ms: u64,
    kind: QueryKind,
) {
    let is_slow = duration_ms >= slow_query_threshold_ms;

    if cfg!(debug_assertions) {
        tracing::debug!(
            query_name,
            duration_ms,
            rows_affected,
            sql = query_sql,
            slow_threshold_ms = slow_query_threshold_ms,
            "query timing"
        );
    } else if is_slow {
        tracing::warn!(
            query_name,
            duration_ms,
            threshold_ms = slow_query_threshold_ms,
            rows_affected,
            sql = query_sql,
            "slow query detected"
        );
    }

    if is_slow {
        SLOW_QUERY_COUNT.fetch_add(1, Ordering::Relaxed);
        crate::metrics::db_slow_queries_total().add(1, &[]);

        // Only capture plans for read queries and when we have a pool.
        if kind == QueryKind::Read {
            if let Some(pool) = pool {
                let owned_name = query_name.to_string();
                let owned_sql = query_sql.to_string();
                tokio::spawn(async move {
                    capture_and_store_plan(
                        &pool,
                        &owned_name,
                        &owned_sql,
                        duration_ms,
                        slow_query_threshold_ms,
                        rows_affected,
                    )
                    .await;
                });
            } else {
                tracing::debug!(
                    query_name,
                    "slow read query detected but no pool supplied — plan not captured"
                );
            }
        } else {
            tracing::info!(
                query_name,
                duration_ms,
                "slow write query detected — plan capture skipped to avoid re-execution"
            );
        }
    }
}

/// Run `EXPLAIN (ANALYZE, BUFFERS, FORMAT TEXT)` for `sql` on a fresh
/// connection from `pool`, then persist the plan to `slow_query_plans`.
///
/// Errors are logged and swallowed — plan capture is best-effort and must
/// never cause the caller to fail.
async fn capture_and_store_plan(
    pool: &PgPool,
    query_name: &str,
    sql: &str,
    duration_ms: u64,
    threshold_ms: u64,
    rows_affected: usize,
) {
    if sql.trim().is_empty() {
        return;
    }

    // Build the EXPLAIN wrapper.  We use a plain-text format (the default)
    // for human readability; JSON is available if structured analysis is
    // ever needed.
    let explain_sql = format!("EXPLAIN (ANALYZE, BUFFERS, FORMAT TEXT) {sql}");

    let plan_result = sqlx::query_scalar::<_, String>(&explain_sql)
        .fetch_all(pool)
        .await;

    let plan_text = match plan_result {
        Ok(rows) => rows.join("\n"),
        Err(e) => {
            tracing::warn!(
                query_name,
                error = %e,
                "failed to capture EXPLAIN ANALYZE plan for slow query"
            );
            return;
        }
    };

    // Truncate oversized plans to keep storage bounded.
    let plan_text = if plan_text.len() > MAX_PLAN_BYTES {
        tracing::warn!(
            query_name,
            original_bytes = plan_text.len(),
            max_bytes = MAX_PLAN_BYTES,
            "plan truncated to MAX_PLAN_BYTES"
        );
        format!(
            "{}\n\n[TRUNCATED: original plan was {} bytes, limit is {}]",
            &plan_text[..MAX_PLAN_BYTES],
            plan_text.len(),
            MAX_PLAN_BYTES
        )
    } else {
        plan_text
    };

    let id = Uuid::new_v4();
    let insert_result = sqlx::query(
        "INSERT INTO slow_query_plans \
         (id, query_name, query_sql, duration_ms, threshold_ms, rows_affected, plan_text) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(id)
    .bind(query_name)
    .bind(sql)
    .bind(duration_ms as i64)
    .bind(threshold_ms as i64)
    .bind(rows_affected as i64)
    .bind(&plan_text)
    .execute(pool)
    .await;

    match insert_result {
        Ok(_) => tracing::info!(
            query_name,
            duration_ms,
            plan_id = %id,
            "slow query plan captured"
        ),
        Err(e) => tracing::warn!(
            query_name,
            error = %e,
            "failed to persist slow query plan"
        ),
    }
}

// ---------------------------------------------------------------------------
// Retention / pruning
// ---------------------------------------------------------------------------

/// Delete `slow_query_plans` rows older than `retention_days`.
///
/// Returns the number of rows pruned.  Call from the cron scheduler to keep
/// the table bounded.  Defaults to [`DEFAULT_RETENTION_DAYS`] if `None`.
///
/// # Errors
/// Returns `sqlx::Error` on database failure; callers should log and continue.
pub async fn prune_old_plans(
    pool: &PgPool,
    retention_days: Option<i64>,
) -> Result<u64, sqlx::Error> {
    let days = retention_days.unwrap_or(DEFAULT_RETENTION_DAYS);
    let result = sqlx::query(
        "DELETE FROM slow_query_plans \
         WHERE captured_at < NOW() - ($1 || ' days')::INTERVAL",
    )
    .bind(days)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

// ---------------------------------------------------------------------------
// Backward-compat helpers
// ---------------------------------------------------------------------------

/// Get the total count of slow queries recorded.
pub fn get_slow_query_count() -> u64 {
    SLOW_QUERY_COUNT.load(Ordering::Relaxed)
}

/// Reset the slow query counter (for testing).
#[cfg(test)]
pub fn reset_slow_query_count() {
    SLOW_QUERY_COUNT.store(0, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// time_query! macro — unchanged public API
// ---------------------------------------------------------------------------

/// Utility macro for measuring query execution time.
/// For plan capture use `log_query_timing_with_plan` directly.
#[macro_export]
macro_rules! time_query {
    ($query_name:expr, $slow_threshold:expr, $block:expr) => {{
        let start = std::time::Instant::now();
        let result = $block;
        let duration_ms = start.elapsed().as_millis() as u64;
        $crate::db::slow_query::log_query_timing(
            $query_name,
            "",
            duration_ms,
            0,
            $slow_threshold,
        );
        result
    }};
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slow_query_counter_increments() {
        reset_slow_query_count();
        assert_eq!(get_slow_query_count(), 0);
        // duration (600) > threshold (500) → slow
        log_query_timing("slow_query", "SELECT 1", 600, 1, 500);
        assert_eq!(get_slow_query_count(), 1);
    }

    #[test]
    fn test_fast_query_does_not_increment_counter() {
        reset_slow_query_count();
        log_query_timing("fast_query", "SELECT 1", 100, 1, 500);
        assert_eq!(get_slow_query_count(), 0);
    }

    #[test]
    fn test_query_kind_write_does_not_capture_plan() {
        // This is a logic-only test — write queries must never be re-executed.
        // We call the inner fn with no pool, kind=Write; the only observable
        // side-effect is the SLOW_QUERY_COUNT increment (no panic, no spawn).
        reset_slow_query_count();
        log_query_timing_inner(
            None,
            "update_tx",
            "UPDATE transactions SET status='failed' WHERE id=$1",
            1000,
            1,
            500,
            QueryKind::Write,
        );
        assert_eq!(get_slow_query_count(), 1, "counter must increment for writes too");
    }

    /// Integration test: triggers a deliberately slow `pg_sleep` query and
    /// asserts that a plan row is captured in `slow_query_plans`.
    ///
    /// Requires Docker (testcontainers).
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_plan_captured_for_slow_read_query() {
        use sqlx::migrate::Migrator;
        use std::path::Path;
        use testcontainers::{runners::AsyncRunner, ImageExt};
        use testcontainers_modules::postgres::Postgres;

        let container = Postgres::default()
            .with_tag("14-alpine")
            .start()
            .await
            .unwrap();
        let port = container.get_host_port_ipv4(5432).await.unwrap();
        let url = format!("postgres://postgres:postgres@127.0.0.1:{}/postgres", port);

        let pool = PgPool::connect(&url).await.unwrap();
        Migrator::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"))
            .await
            .unwrap()
            .run(&pool)
            .await
            .unwrap();

        // Use a threshold of 0 ms so any query counts as "slow".
        let threshold_ms: u64 = 0;
        // pg_sleep is a safe read-only function — safe for EXPLAIN re-execution.
        let sql = "SELECT pg_sleep(0.01)";

        // Simulate a slow query being reported.
        log_query_timing_with_plan(
            &pool,
            "test_slow_select",
            sql,
            50, // reported duration
            1,
            threshold_ms,
            QueryKind::Read,
        );

        // Give the spawned task time to complete.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Assert the plan was stored.
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM slow_query_plans WHERE query_name = $1")
                .bind("test_slow_select")
                .fetch_one(&pool)
                .await
                .unwrap();

        assert!(count >= 1, "expected at least one plan row, got {count}");

        // Assert the plan text is non-empty and looks like an EXPLAIN output.
        let plan_text: String =
            sqlx::query_scalar("SELECT plan_text FROM slow_query_plans WHERE query_name = $1 LIMIT 1")
                .bind("test_slow_select")
                .fetch_one(&pool)
                .await
                .unwrap();

        assert!(
            !plan_text.is_empty(),
            "plan_text must not be empty"
        );
        assert!(
            plan_text.contains("Planning Time") || plan_text.contains("Execution Time") || plan_text.contains("Result"),
            "plan_text should contain EXPLAIN output keywords, got: {plan_text}"
        );
    }

    /// Asserts that write queries are not re-executed even when a pool is
    /// supplied — the plan row count for the write query must stay at 0.
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_no_plan_captured_for_write_query() {
        use sqlx::migrate::Migrator;
        use std::path::Path;
        use testcontainers::{runners::AsyncRunner, ImageExt};
        use testcontainers_modules::postgres::Postgres;

        let container = Postgres::default()
            .with_tag("14-alpine")
            .start()
            .await
            .unwrap();
        let port = container.get_host_port_ipv4(5432).await.unwrap();
        let url = format!("postgres://postgres:postgres@127.0.0.1:{}/postgres", port);

        let pool = PgPool::connect(&url).await.unwrap();
        Migrator::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"))
            .await
            .unwrap()
            .run(&pool)
            .await
            .unwrap();

        log_query_timing_with_plan(
            &pool,
            "write_no_plan",
            "UPDATE transactions SET status='failed' WHERE id=$1",
            200,
            0,
            0, // threshold 0 → always "slow"
            QueryKind::Write,
        );

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM slow_query_plans WHERE query_name = $1")
                .bind("write_no_plan")
                .fetch_one(&pool)
                .await
                .unwrap();

        assert_eq!(count, 0, "write queries must never have a plan captured");
    }

    /// Asserts that prune_old_plans removes rows older than the retention window.
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_prune_old_plans() {
        use sqlx::migrate::Migrator;
        use std::path::Path;
        use testcontainers::{runners::AsyncRunner, ImageExt};
        use testcontainers_modules::postgres::Postgres;

        let container = Postgres::default()
            .with_tag("14-alpine")
            .start()
            .await
            .unwrap();
        let port = container.get_host_port_ipv4(5432).await.unwrap();
        let url = format!("postgres://postgres:postgres@127.0.0.1:{}/postgres", port);

        let pool = PgPool::connect(&url).await.unwrap();
        Migrator::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"))
            .await
            .unwrap()
            .run(&pool)
            .await
            .unwrap();

        // Insert a row backdated 40 days.
        sqlx::query(
            "INSERT INTO slow_query_plans \
             (id, query_name, query_sql, duration_ms, threshold_ms, plan_text, captured_at) \
             VALUES ($1, 'old_plan', 'SELECT 1', 999, 500, 'fake plan', NOW() - INTERVAL '40 days')",
        )
        .bind(Uuid::new_v4())
        .execute(&pool)
        .await
        .unwrap();

        // Insert a recent row (should survive pruning).
        sqlx::query(
            "INSERT INTO slow_query_plans \
             (id, query_name, query_sql, duration_ms, threshold_ms, plan_text) \
             VALUES ($1, 'recent_plan', 'SELECT 2', 999, 500, 'fake plan')",
        )
        .bind(Uuid::new_v4())
        .execute(&pool)
        .await
        .unwrap();

        let pruned = prune_old_plans(&pool, Some(30)).await.unwrap();
        assert_eq!(pruned, 1, "expected 1 old row pruned, got {pruned}");

        let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM slow_query_plans")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(remaining, 1, "recent row must survive pruning");
    }
}
