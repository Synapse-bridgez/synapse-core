//! Time and identifier sources for the Swap Engine.
//!
//! The engine never calls `Utc::now()` or `Uuid::new_v4()` directly; it asks
//! a [`Clock`] and an [`IdGenerator`]. Production uses [`SystemClock`] and
//! [`UuidIdGenerator`]; tests and the end-to-end harness plug in
//! [`ManualClock`] and [`SequentialIdGenerator`] so every run of a scenario
//! produces byte-identical quotes, swap records and event logs.

use chrono::{DateTime, Duration, Utc};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

pub trait IdGenerator: Send + Sync {
    /// A new unique id, prefixed (`quote_...`, `swap_...`).
    fn next_id(&self, prefix: &str) -> String;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct UuidIdGenerator;

impl IdGenerator for UuidIdGenerator {
    fn next_id(&self, prefix: &str) -> String {
        format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
    }
}

/// A clock that only moves when told to.
#[derive(Debug)]
pub struct ManualClock {
    now: Mutex<DateTime<Utc>>,
}

impl ManualClock {
    pub fn new(start: DateTime<Utc>) -> Self {
        Self {
            now: Mutex::new(start),
        }
    }

    pub fn advance(&self, by: Duration) {
        let mut now = self.now.lock().unwrap_or_else(|e| e.into_inner());
        *now += by;
    }

    pub fn set(&self, to: DateTime<Utc>) {
        *self.now.lock().unwrap_or_else(|e| e.into_inner()) = to;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// `prefix_000001`, `prefix_000002`, ... (one counter shared by all
/// prefixes).
#[derive(Debug, Default)]
pub struct SequentialIdGenerator {
    next: AtomicU64,
}

impl IdGenerator for SequentialIdGenerator {
    fn next_id(&self, prefix: &str) -> String {
        let n = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        format!("{prefix}_{n:06}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_and_sequential_ids_are_deterministic() {
        let start = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let c = ManualClock::new(start);
        c.advance(Duration::seconds(30));
        assert_eq!(c.now(), start + Duration::seconds(30));
        let ids = SequentialIdGenerator::default();
        assert_eq!(ids.next_id("quote"), "quote_000001");
        assert_eq!(ids.next_id("swap"), "swap_000002");
    }
}
