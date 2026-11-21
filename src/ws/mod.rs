//! WebSocket module for real-time events.
//!
//! # Graceful Shutdown
//!
//! WebSocket handlers participate in the application shutdown path owned by
//! `main.rs`. When the process receives SIGTERM or SIGINT, the HTTP server
//! starts readiness draining before it stops accepting new work. WebSocket
//! endpoints should treat that drain signal as the point where no new upgrade
//! requests are admitted and existing real-time streams are allowed to close
//! cleanly.
//!
//! The expected shutdown sequence is:
//!
//! 1. Mark readiness as draining so load balancers stop routing new clients.
//! 2. Stop accepting new WebSocket upgrades by respecting the shared readiness
//!    state before acquiring a connection permit.
//! 3. Notify active clients with a normal close frame when the handler has a
//!    shutdown signal available.
//! 4. Drop each `ConnectionPermit` exactly once as the handler exits so pool
//!    accounting returns to zero.
//! 5. Let clients reconnect and use the documented resync flow to recover any
//!    events missed during the drain window.
//!
//! # Security
//!
//! Shutdown handling must not bypass authentication or tenant authorization.
//! Draining clients should receive only client-safe close reasons, and logs may
//! carry operational context without including bearer tokens, raw messages, or
//! tenant data from payloads.
//!
//! # Performance
//!
//! The drain path should avoid unbounded waits and unbounded per-connection
//! buffers. Long-running send loops should select on their normal work, health
//! checks, and shutdown notification so slow clients cannot hold server
//! termination indefinitely.
pub mod connection_pool;
pub mod health;
pub mod metrics;
pub mod pagination;

pub use connection_pool::ConnectionPool;
pub use health::HealthChecker;
pub use metrics::WebSocketMetrics;
pub use pagination::{
    Cursor, EventPage, PaginationError, PaginationParams, PaginationRequest, DEFAULT_PAGE_SIZE,
    MAX_PAGE_SIZE,
};

/// Maximum number of focus targets remembered per connection.
pub const MAX_TRACKED_FOCUS_TARGETS: usize = 64;

/// Maximum accepted length of a single focus target identifier.
pub const MAX_FOCUS_TARGET_LEN: usize = 256;

/// Per-connection keyboard focus tracking.
///
/// A client reports the UI element it currently has keyboard focus on with a
/// `focus` message. The last reported target is remembered so that a
/// reconnect acknowledgement can tell the client where to restore focus.
/// State is keyed by the client-supplied `session_id` so it can be looked up
/// again after the socket is re-established.
#[derive(Debug, Default)]
pub struct FocusTracker {
    last_targets: std::collections::HashMap<String, String>,
}

impl FocusTracker {
    /// Creates an empty tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records `target` as the last known focus target for `session_id`.
    ///
    /// Returns `false` when the target is rejected (empty, too long, or the
    /// tracker is full of other sessions).
    pub fn record(&mut self, session_id: &str, target: &str) -> bool {
        if target.is_empty() || target.len() > MAX_FOCUS_TARGET_LEN {
            return false;
        }
        if !self.last_targets.contains_key(session_id)
            && self.last_targets.len() >= MAX_TRACKED_FOCUS_TARGETS
        {
            return false;
        }
        self.last_targets
            .insert(session_id.to_string(), target.to_string());
        true
    }

    /// Returns the last known focus target for `session_id`, if any.
    pub fn last_target(&self, session_id: &str) -> Option<&str> {
        self.last_targets.get(session_id).map(String::as_str)
    }

    /// Forgets the focus target for `session_id`.
    pub fn clear(&mut self, session_id: &str) {
        self.last_targets.remove(session_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ws_module_loads() {
        // Module loads successfully
    }

    #[test]
    fn test_focus_tracker_records_last_target() {
        let mut tracker = FocusTracker::new();
        assert!(tracker.record("session-1", "search-input"));
        assert!(tracker.record("session-1", "tx-table-row-7"));
        assert_eq!(tracker.last_target("session-1"), Some("tx-table-row-7"));
    }

    #[test]
    fn test_focus_tracker_isolates_sessions() {
        let mut tracker = FocusTracker::new();
        tracker.record("session-1", "search-input");
        tracker.record("session-2", "filter-select");
        assert_eq!(tracker.last_target("session-1"), Some("search-input"));
        assert_eq!(tracker.last_target("session-2"), Some("filter-select"));
        assert_eq!(tracker.last_target("session-3"), None);
    }

    #[test]
    fn test_focus_tracker_rejects_invalid_targets() {
        let mut tracker = FocusTracker::new();
        assert!(!tracker.record("session-1", ""));
        assert!(!tracker.record("session-1", &"x".repeat(MAX_FOCUS_TARGET_LEN + 1)));
        assert_eq!(tracker.last_target("session-1"), None);
    }

    #[test]
    fn test_focus_tracker_clear() {
        let mut tracker = FocusTracker::new();
        tracker.record("session-1", "search-input");
        tracker.clear("session-1");
        assert_eq!(tracker.last_target("session-1"), None);
    }
}
