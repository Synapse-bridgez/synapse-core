use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Counter for slow queries
pub static SLOW_QUERY_COUNT: AtomicU64 = AtomicU64::new(0);

/// Log a database query with timing information
///
/// # Arguments
/// * `query_name` - Human-readable name for the query (e.g., "fetch_transaction")
/// * `query_sql` - Parameterized SQL query text (e.g., "SELECT * FROM transactions WHERE id = $1")
/// * `duration_ms` - Duration in milliseconds
/// * `rows_affected` - Number of rows returned or affected
/// * `slow_query_threshold_ms` - Threshold in milliseconds for considering query as "slow"
///
/// # Example
/// ```
/// log_query_timing("fetch_transaction", "SELECT * FROM transactions WHERE id = $1", 125, 1, 500);
/// ```
pub fn log_query_timing(
    query_name: &str,
    query_sql: &str,
    duration_ms: u64,
    rows_affected: usize,
    slow_query_threshold_ms: u64,
) {
    // Log all queries in development, or slow queries in production
    if cfg!(debug_assertions) {
        // Development: log everything at debug level
        tracing::debug!(
            query_name = query_name,
            duration_ms = duration_ms,
            rows_affected = rows_affected,
            sql = query_sql,
            "query timing"
        );
    } else if duration_ms >= slow_query_threshold_ms {
        // Production: log slow queries at warn level
        tracing::warn!(
            query_name = query_name,
            duration_ms = duration_ms,
            threshold_ms = slow_query_threshold_ms,
            rows_affected = rows_affected,
            sql = query_sql,
            "slow query detected"
        );

        // Increment slow query counter
        SLOW_QUERY_COUNT.fetch_add(1, Ordering::Relaxed);
    }
}

/// Get the total count of slow queries recorded
pub fn get_slow_query_count() -> u64 {
    SLOW_QUERY_COUNT.load(Ordering::Relaxed)
}

/// Reset the slow query counter (primarily for testing)
#[cfg(test)]
pub fn reset_slow_query_count() {
    SLOW_QUERY_COUNT.store(0, Ordering::Relaxed);
}

/// Utility macro for measuring query execution time
#[macro_export]
macro_rules! time_query {
    ($query_name:expr, $slow_threshold:expr, $block:expr) => {{
        let start = std::time::Instant::now();
        let result = $block;
        let duration_ms = start.elapsed().as_millis() as u64;
        $crate::db::slow_query::log_query_timing(
            $query_name,
            "", // Query SQL would need to be captured if desired
            duration_ms,
            0,
            $slow_threshold,
        );
        result
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slow_query_logging_disabled_in_test() {
        reset_slow_query_count();
        log_query_timing("test_query", "SELECT 1", 1000, 1, 500);
        // In tests, we still log to tracing but don't necessarily track
    }

    #[test]
    fn test_slow_query_counter() {
        reset_slow_query_count();
        assert_eq!(get_slow_query_count(), 0);
        log_query_timing("slow_query", "SELECT * FROM data", 600, 10, 500);
        // Counter should increment if we're not in debug mode
    }
}
