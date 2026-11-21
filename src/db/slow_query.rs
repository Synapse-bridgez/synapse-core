use std::sync::atomic::{AtomicU64, Ordering};

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

    // Development: log every query in debug builds.
    // Production: only record slow query events.
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
        tracing::warn!(
            query_name = query_name,
            duration_ms = duration_ms,
            threshold_ms = slow_query_threshold_ms,
            rows_affected = rows_affected,
            sql = query_sql,
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

/// Timing handle for a database query.
///
/// The clock is only started once [`QueryTimer::start`] is called, which lets
/// callers perform cache lookups first and short-circuit on a cache hit without
/// ever starting the timer. Only actual database execution time is therefore
/// counted toward the slow-query threshold.
#[derive(Debug, Clone, Copy)]
pub struct QueryTimer {
    started_at: Option<std::time::Instant>,
}

impl QueryTimer {
    /// Creates a timer that has not started yet.
    pub const fn new() -> Self {
        Self { started_at: None }
    }

    /// Starts the timer. Call this immediately before executing the query.
    pub fn start(&mut self) {
        self.started_at = Some(std::time::Instant::now());
    }

    /// Returns `true` once the timer has been started.
    pub fn is_started(&self) -> bool {
        self.started_at.is_some()
    }

    /// Elapsed milliseconds since [`QueryTimer::start`], or `None` if the timer
    /// was never started (e.g. the query was served from cache).
    pub fn elapsed_ms(&self) -> Option<u64> {
        self.started_at
            .map(|start| start.elapsed().as_millis() as u64)
    }

    /// Records the timing if the timer was started.
    ///
    /// Returns `true` when a slow-query entry was recorded. A timer that was
    /// never started (cache hit) never records anything, regardless of wall
    /// time.
    pub fn finish(
        &self,
        query_name: &str,
        query_sql: &str,
        rows_affected: usize,
        slow_query_threshold_ms: u64,
    ) -> bool {
        match self.elapsed_ms() {
            Some(duration_ms) => {
                log_query_timing(
                    query_name,
                    query_sql,
                    duration_ms,
                    rows_affected,
                    slow_query_threshold_ms,
                );
                duration_ms >= slow_query_threshold_ms
            }
            None => false,
        }
    }
}

impl Default for QueryTimer {
    fn default() -> Self {
        Self::new()
    }
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

    #[test]
    fn test_unstarted_timer_never_records_slow_query() {
        reset_slow_query_count();
        let timer = QueryTimer::new();
        assert!(!timer.is_started());
        assert_eq!(timer.elapsed_ms(), None);
        // Even with a zero threshold, an unstarted timer (cache hit) records nothing.
        assert!(!timer.finish("cached_query", "SELECT 1", 1, 0));
        assert_eq!(get_slow_query_count(), 0);
    }

    #[test]
    fn test_started_timer_records_timing() {
        let mut timer = QueryTimer::new();
        timer.start();
        assert!(timer.is_started());
        assert!(timer.elapsed_ms().is_some());
        // Threshold of u64::MAX means the query is never considered slow.
        assert!(!timer.finish("fast_query", "SELECT 1", 1, u64::MAX));
    }
}
