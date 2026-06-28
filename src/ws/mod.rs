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

#[cfg(test)]
mod tests {
    #[test]
    fn test_ws_module_loads() {
        // Module loads successfully
    }
}
