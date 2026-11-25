use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

/// Counter for slow queries
pub static SLOW_QUERY_COUNT: AtomicU64 = AtomicU64::new(0);

/// Compute a short stable hash of the SQL statement text.
/// Returns the first 16 hex characters of a 64-bit hash, which is enough to
/// group occurrences without exposing parameter values or bloating log storage.
fn sql_hash(sql: &str) -> String {
    let mut hasher = DefaultHasher::new();
    sql.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Log a database query with timing information.
///
/// For slow queries the full SQL text is **never** written to the warn log;
/// only a short hash of the statement is recorded so that:
///  - occurrences of the same query pattern can be correlated, and
///  - parameter values and PII are not leaked into log storage.
///
/// The full SQL text is still written at `debug` level (enabled only when
/// `debug_assertions` is active, i.e. local dev builds) so debugging remains
/// convenient without changing production behaviour.
///
/// # Arguments
/// * `query_name` - Human-readable name for the query (e.g., "fetch_transaction")
/// * `query_sql` - Parameterized SQL query text (e.g., "SELECT * FROM transactions WHERE id = $1")
/// * `duration_ms` - Duration in milliseconds
/// * `rows_affected` - Number of rows returned or affected
/// * `slow_query_threshold_ms` - Threshold in milliseconds for considering query as "slow"
///
/// # Example
/// ```rust,no_run
/// use synapse_core::db::slow_query::log_query_timing;
/// log_query_timing("fetch_transaction", "SELECT * FROM transactions WHERE id = $1", 125, 1, 500);
/// ```
pub fn log_query_timing(
    query_name: &str,
    query_sql: &str,
    duration_ms: u64,
    rows_affected: usize,
    slow_query_threshold_ms: u64,
) {
    let is_slow = duration_ms >= slow_query_threshold_ms;

    // Development: log every query with the full SQL text in debug builds so
    // local debugging still works without changing production behaviour.
    if cfg!(debug_assertions) {
        tracing::debug!(
            query_name = query_name,
            duration_ms = duration_ms,
            rows_affected = rows_affected,
            sql = query_sql,
            slow_threshold_ms = slow_query_threshold_ms,
            "query timing"
        );
    } else if is_slow {
        // Production slow-query log: record the hash, not the full statement.
        // The hash is stable across runs for the same query text, so it can be
        // used to correlate occurrences and join against an out-of-band query
        // catalogue if needed.
        let hash = sql_hash(query_sql);
        tracing::warn!(
            query_name = query_name,
            duration_ms = duration_ms,
            threshold_ms = slow_query_threshold_ms,
            rows_affected = rows_affected,
            sql_hash = %hash,
            "slow query detected"
        );
    }

    if is_slow {
        // Track slow query count both in-process and via OpenTelemetry.
        SLOW_QUERY_COUNT.fetch_add(1, Ordering::Relaxed);
        crate::metrics::db_slow_queries_total().add(1, &[]);
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
    fn test_sql_hash_is_deterministic() {
        let sql = "SELECT * FROM transactions WHERE id = $1";
        assert_eq!(sql_hash(sql), sql_hash(sql));
    }

    #[test]
    fn test_sql_hash_differs_for_different_queries() {
        let a = sql_hash("SELECT 1");
        let b = sql_hash("SELECT 2");
        assert_ne!(a, b);
    }

    #[test]
    fn test_sql_hash_is_16_hex_chars() {
        let h = sql_hash("SELECT * FROM tenants WHERE id = $1");
        assert_eq!(h.len(), 16);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

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
