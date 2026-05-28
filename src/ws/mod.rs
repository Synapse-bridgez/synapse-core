/// WebSocket module for real-time events
pub mod health;
pub mod connection_pool;

pub use health::HealthChecker;
pub use connection_pool::ConnectionPool;

#[cfg(test)]
mod tests {
    #[test]
    fn test_ws_module_loads() {
        // Module loads successfully
    }
}
