//! Injectable clock for the swap engine (#1519).
//!
//! Convention: all wall-clock time is UTC (`DateTime<Utc>`), matching the
//! storage timezone. Business dates / expiries use [`Clock::now`]; timeouts and
//! durations must use [`Clock::monotonic`], which never goes backwards.
//! Swap code must take a `&dyn Clock` (via application state) instead of
//! calling `Utc::now()` / `Instant::now()` directly; `swap_code_avoids_global_clock`
//! enforces this.
//!
//! The mock clock only advances *logical* time. Database-visible timestamps
//! (`NOW()` in SQL) are not affected - tests that touch partitions should pass
//! the clock's `now()` as an explicit parameter instead of relying on `NOW()`.

use chrono::{DateTime, Duration, Utc};
use std::sync::Mutex;
use std::time::Instant;

pub trait Clock: Send + Sync {
    /// Wall-clock UTC time, for business dates and expiries.
    fn now(&self) -> DateTime<Utc>;
    /// Monotonic offset since the clock was created, for timeouts.
    fn monotonic(&self) -> std::time::Duration;
}

/// Production clock backed by the system clock.
pub struct SystemClock {
    start: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
    fn monotonic(&self) -> std::time::Duration {
        self.start.elapsed()
    }
}

/// Manually advanced clock for deterministic tests.
pub struct MockClock {
    state: Mutex<(DateTime<Utc>, std::time::Duration)>,
}

impl MockClock {
    pub fn new(start: DateTime<Utc>) -> Self {
        Self {
            state: Mutex::new((start, std::time::Duration::ZERO)),
        }
    }

    /// Advance both wall-clock and monotonic time.
    pub fn advance(&self, by: Duration) {
        let mut s = self.state.lock().unwrap();
        s.0 += by;
        s.1 += by.to_std().unwrap_or_default();
    }

    /// Advance in `step`-sized ticks up to `total`, calling `on_tick` after each
    /// tick so callers can drive pending timers (e.g. fast-forward months of
    /// partition/retention maintenance).
    pub fn advance_in_steps(
        &self,
        total: Duration,
        step: Duration,
        mut on_tick: impl FnMut(DateTime<Utc>),
    ) {
        let mut remaining = total;
        while remaining > Duration::zero() {
            let d = step.min(remaining);
            self.advance(d);
            remaining -= d;
            on_tick(self.now());
        }
    }

    /// Jump wall-clock time only (e.g. DST/NTP step); monotonic time is unchanged.
    pub fn set_wall(&self, to: DateTime<Utc>) {
        self.state.lock().unwrap().0 = to;
    }
}

impl Clock for MockClock {
    fn now(&self) -> DateTime<Utc> {
        self.state.lock().unwrap().0
    }
    fn monotonic(&self) -> std::time::Duration {
        self.state.lock().unwrap().1
    }
}

/// True when `expires_at` has passed. The boundary instant itself is expired.
pub fn is_expired(clock: &dyn Clock, expires_at: DateTime<Utc>) -> bool {
    clock.now() >= expires_at
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(y: i32, m: u32, d: u32, h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, 0, 0).unwrap()
    }

    #[test]
    fn quote_expiry_exactly_at_boundary() {
        let clock = MockClock::new(t(2026, 1, 1, 0));
        let expires = t(2026, 1, 1, 0) + Duration::seconds(30);
        clock.advance(Duration::seconds(29) + Duration::milliseconds(999));
        assert!(!is_expired(&clock, expires));
        clock.advance(Duration::milliseconds(1));
        assert!(is_expired(&clock, expires));
    }

    #[test]
    fn month_end_rollover_and_long_horizon() {
        let clock = MockClock::new(t(2026, 1, 31, 23));
        clock.advance(Duration::hours(1));
        assert_eq!(clock.now(), t(2026, 2, 1, 0));

        let mut months = std::collections::BTreeSet::new();
        clock.advance_in_steps(Duration::days(180), Duration::days(1), |now| {
            months.insert(now.format("%Y-%m").to_string());
        });
        assert_eq!(months.len(), 6); // 2026-02 .. 2026-07
    }

    #[test]
    fn retention_cutoff_follows_clock() {
        let clock = MockClock::new(t(2026, 1, 1, 0));
        let created = clock.now();
        let retention = Duration::days(90);
        assert!(clock.now() - retention < created);
        clock.advance(Duration::days(91));
        assert!(clock.now() - retention > created);
    }

    #[test]
    fn wall_jump_does_not_move_monotonic() {
        let clock = MockClock::new(t(2026, 3, 8, 6));
        clock.advance(Duration::seconds(10));
        clock.set_wall(t(2026, 3, 8, 7)); // DST-style forward step
        assert_eq!(clock.monotonic(), std::time::Duration::from_secs(10));
        assert_eq!(clock.now(), t(2026, 3, 8, 7));
    }

    #[test]
    fn timeout_race_uses_monotonic() {
        let clock = MockClock::new(t(2026, 1, 1, 0));
        let started = clock.monotonic();
        let timeout = std::time::Duration::from_secs(5);
        clock.advance(Duration::seconds(5));
        assert!(clock.monotonic() - started >= timeout);
    }

    #[test]
    fn swap_code_avoids_global_clock() {
        let src = include_str!("../swap/mod.rs");
        let non_test = src.split("#[cfg(test)]").next().unwrap();
        for banned in ["Utc::now(", "Instant::now(", "SystemTime::now("] {
            assert!(
                !non_test.contains(banned),
                "swap code must use Clock, found {banned}"
            );
        }
        let tests = src.split("#[cfg(test)]").nth(1).unwrap_or("");
        assert!(!tests.contains("Utc::now("), "swap tests must use a Clock");
    }
}
