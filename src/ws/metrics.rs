//! Metrics collection for WebSocket operations.
//! Tracks connection events, message throughput, and performance metrics.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// WebSocket metrics collector
#[derive(Debug, Clone)]
pub struct WebSocketMetrics {
    /// Total number of connections established
    total_connections: Arc<AtomicU64>,
    /// Number of active connections
    active_connections: Arc<AtomicU64>,
    /// Total number of messages sent
    messages_sent: Arc<AtomicU64>,
    /// Total number of messages received
    messages_received: Arc<AtomicU64>,
    /// Total number of connection errors
    connection_errors: Arc<AtomicU64>,
    /// Total number of message errors
    message_errors: Arc<AtomicU64>,
    /// Total bytes sent
    bytes_sent: Arc<AtomicU64>,
    /// Total bytes received
    bytes_received: Arc<AtomicU64>,
    /// Timestamp of last connection
    last_connection_time: Arc<parking_lot::Mutex<Option<Instant>>>,
    /// Timestamp of last message
    last_message_time: Arc<parking_lot::Mutex<Option<Instant>>>,
}

impl WebSocketMetrics {
    /// Creates a new metrics collector
    pub fn new() -> Self {
        Self {
            total_connections: Arc::new(AtomicU64::new(0)),
            active_connections: Arc::new(AtomicU64::new(0)),
            messages_sent: Arc::new(AtomicU64::new(0)),
            messages_received: Arc::new(AtomicU64::new(0)),
            connection_errors: Arc::new(AtomicU64::new(0)),
            message_errors: Arc::new(AtomicU64::new(0)),
            bytes_sent: Arc::new(AtomicU64::new(0)),
            bytes_received: Arc::new(AtomicU64::new(0)),
            last_connection_time: Arc::new(parking_lot::Mutex::new(None)),
            last_message_time: Arc::new(parking_lot::Mutex::new(None)),
        }
    }

    /// Records a new connection
    pub fn record_connection(&self) {
        self.total_connections.fetch_add(1, Ordering::Relaxed);
        self.active_connections.fetch_add(1, Ordering::Relaxed);
        *self.last_connection_time.lock() = Some(Instant::now());
    }

    /// Records a disconnection
    pub fn record_disconnection(&self) {
        self.active_connections.fetch_sub(1, Ordering::Relaxed);
    }

    /// Records a sent message
    pub fn record_message_sent(&self, bytes: u64) {
        self.messages_sent.fetch_add(1, Ordering::Relaxed);
        self.bytes_sent.fetch_add(bytes, Ordering::Relaxed);
        *self.last_message_time.lock() = Some(Instant::now());
    }

    /// Records a received message
    pub fn record_message_received(&self, bytes: u64) {
        self.messages_received.fetch_add(1, Ordering::Relaxed);
        self.bytes_received.fetch_add(bytes, Ordering::Relaxed);
        *self.last_message_time.lock() = Some(Instant::now());
    }

    /// Records a connection error
    pub fn record_connection_error(&self) {
        self.connection_errors.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a message error
    pub fn record_message_error(&self) {
        self.message_errors.fetch_add(1, Ordering::Relaxed);
    }

    /// Gets the total number of connections
    pub fn total_connections(&self) -> u64 {
        self.total_connections.load(Ordering::Relaxed)
    }

    /// Gets the number of active connections
    pub fn active_connections(&self) -> u64 {
        self.active_connections.load(Ordering::Relaxed)
    }

    /// Gets the total number of messages sent
    pub fn messages_sent(&self) -> u64 {
        self.messages_sent.load(Ordering::Relaxed)
    }

    /// Gets the total number of messages received
    pub fn messages_received(&self) -> u64 {
        self.messages_received.load(Ordering::Relaxed)
    }

    /// Gets the total number of connection errors
    pub fn connection_errors(&self) -> u64 {
        self.connection_errors.load(Ordering::Relaxed)
    }

    /// Gets the total number of message errors
    pub fn message_errors(&self) -> u64 {
        self.message_errors.load(Ordering::Relaxed)
    }

    /// Gets the total bytes sent
    pub fn bytes_sent(&self) -> u64 {
        self.bytes_sent.load(Ordering::Relaxed)
    }

    /// Gets the total bytes received
    pub fn bytes_received(&self) -> u64 {
        self.bytes_received.load(Ordering::Relaxed)
    }

    /// Gets the time since last connection
    pub fn time_since_last_connection(&self) -> Option<Duration> {
        self.last_connection_time
            .lock()
            .map(|t| t.elapsed())
    }

    /// Gets the time since last message
    pub fn time_since_last_message(&self) -> Option<Duration> {
        self.last_message_time
            .lock()
            .map(|t| t.elapsed())
    }

    /// Calculates the error rate as a percentage (0-100)
    pub fn error_rate(&self) -> f64 {
        let total_messages = self.messages_sent() + self.messages_received();
        if total_messages == 0 {
            return 0.0;
        }
        let total_errors = self.connection_errors() + self.message_errors();
        (total_errors as f64 / total_messages as f64) * 100.0
    }

    /// Calculates the average message size in bytes
    pub fn average_message_size(&self) -> f64 {
        let total_messages = self.messages_sent() + self.messages_received();
        if total_messages == 0 {
            return 0.0;
        }
        let total_bytes = self.bytes_sent() + self.bytes_received();
        total_bytes as f64 / total_messages as f64
    }

    /// Resets all metrics to zero
    pub fn reset(&self) {
        self.total_connections.store(0, Ordering::Relaxed);
        self.active_connections.store(0, Ordering::Relaxed);
        self.messages_sent.store(0, Ordering::Relaxed);
        self.messages_received.store(0, Ordering::Relaxed);
        self.connection_errors.store(0, Ordering::Relaxed);
        self.message_errors.store(0, Ordering::Relaxed);
        self.bytes_sent.store(0, Ordering::Relaxed);
        self.bytes_received.store(0, Ordering::Relaxed);
        *self.last_connection_time.lock() = None;
        *self.last_message_time.lock() = None;
    }
}

impl Default for WebSocketMetrics {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_creation() {
        let metrics = WebSocketMetrics::new();
        assert_eq!(metrics.total_connections(), 0);
        assert_eq!(metrics.active_connections(), 0);
        assert_eq!(metrics.messages_sent(), 0);
        assert_eq!(metrics.messages_received(), 0);
        assert_eq!(metrics.connection_errors(), 0);
        assert_eq!(metrics.message_errors(), 0);
    }

    #[test]
    fn test_record_connection() {
        let metrics = WebSocketMetrics::new();
        metrics.record_connection();
        assert_eq!(metrics.total_connections(), 1);
        assert_eq!(metrics.active_connections(), 1);
    }

    #[test]
    fn test_record_disconnection() {
        let metrics = WebSocketMetrics::new();
        metrics.record_connection();
        metrics.record_disconnection();
        assert_eq!(metrics.total_connections(), 1);
        assert_eq!(metrics.active_connections(), 0);
    }

    #[test]
    fn test_record_message_sent() {
        let metrics = WebSocketMetrics::new();
        metrics.record_message_sent(100);
        assert_eq!(metrics.messages_sent(), 1);
        assert_eq!(metrics.bytes_sent(), 100);
    }

    #[test]
    fn test_record_message_received() {
        let metrics = WebSocketMetrics::new();
        metrics.record_message_received(50);
        assert_eq!(metrics.messages_received(), 1);
        assert_eq!(metrics.bytes_received(), 50);
    }

    #[test]
    fn test_record_connection_error() {
        let metrics = WebSocketMetrics::new();
        metrics.record_connection_error();
        assert_eq!(metrics.connection_errors(), 1);
    }

    #[test]
    fn test_record_message_error() {
        let metrics = WebSocketMetrics::new();
        metrics.record_message_error();
        assert_eq!(metrics.message_errors(), 1);
    }

    #[test]
    fn test_multiple_connections() {
        let metrics = WebSocketMetrics::new();
        metrics.record_connection();
        metrics.record_connection();
        metrics.record_connection();
        assert_eq!(metrics.total_connections(), 3);
        assert_eq!(metrics.active_connections(), 3);
    }

    #[test]
    fn test_connection_lifecycle() {
        let metrics = WebSocketMetrics::new();
        metrics.record_connection();
        metrics.record_message_sent(100);
        metrics.record_message_received(50);
        metrics.record_disconnection();
        
        assert_eq!(metrics.total_connections(), 1);
        assert_eq!(metrics.active_connections(), 0);
        assert_eq!(metrics.messages_sent(), 1);
        assert_eq!(metrics.messages_received(), 1);
    }

    #[test]
    fn test_error_rate() {
        let metrics = WebSocketMetrics::new();
        metrics.record_message_sent(100);
        metrics.record_message_error();
        metrics.record_message_received(50);
        
        // 1 error / 2 messages = 50%
        assert_eq!(metrics.error_rate(), 50.0);
    }

    #[test]
    fn test_error_rate_zero_messages() {
        let metrics = WebSocketMetrics::new();
        assert_eq!(metrics.error_rate(), 0.0);
    }

    #[test]
    fn test_average_message_size() {
        let metrics = WebSocketMetrics::new();
        metrics.record_message_sent(100);
        metrics.record_message_received(50);
        
        // (100 + 50) / 2 = 75
        assert_eq!(metrics.average_message_size(), 75.0);
    }

    #[test]
    fn test_average_message_size_zero_messages() {
        let metrics = WebSocketMetrics::new();
        assert_eq!(metrics.average_message_size(), 0.0);
    }

    #[test]
    fn test_time_since_last_connection() {
        let metrics = WebSocketMetrics::new();
        assert!(metrics.time_since_last_connection().is_none());
        
        metrics.record_connection();
        let elapsed = metrics.time_since_last_connection();
        assert!(elapsed.is_some());
        assert!(elapsed.unwrap().as_millis() < 100);
    }

    #[test]
    fn test_time_since_last_message() {
        let metrics = WebSocketMetrics::new();
        assert!(metrics.time_since_last_message().is_none());
        
        metrics.record_message_sent(100);
        let elapsed = metrics.time_since_last_message();
        assert!(elapsed.is_some());
        assert!(elapsed.unwrap().as_millis() < 100);
    }

    #[test]
    fn test_reset() {
        let metrics = WebSocketMetrics::new();
        metrics.record_connection();
        metrics.record_message_sent(100);
        metrics.record_connection_error();
        
        metrics.reset();
        
        assert_eq!(metrics.total_connections(), 0);
        assert_eq!(metrics.active_connections(), 0);
        assert_eq!(metrics.messages_sent(), 0);
        assert_eq!(metrics.connection_errors(), 0);
        assert!(metrics.time_since_last_connection().is_none());
        assert!(metrics.time_since_last_message().is_none());
    }

    #[test]
    fn test_metrics_clone() {
        let metrics = WebSocketMetrics::new();
        metrics.record_connection();
        metrics.record_message_sent(100);
        
        let cloned = metrics.clone();
        assert_eq!(cloned.total_connections(), 1);
        assert_eq!(cloned.messages_sent(), 1);
        
        cloned.record_message_received(50);
        assert_eq!(metrics.messages_received(), 1);
    }

    #[test]
    fn test_concurrent_updates() {
        let metrics = Arc::new(WebSocketMetrics::new());
        let mut handles = vec![];
        
        for _ in 0..10 {
            let metrics_clone = Arc::clone(&metrics);
            let handle = std::thread::spawn(move || {
                metrics_clone.record_connection();
                metrics_clone.record_message_sent(10);
                metrics_clone.record_message_received(5);
            });
            handles.push(handle);
        }
        
        for handle in handles {
            handle.join().unwrap();
        }
        
        assert_eq!(metrics.total_connections(), 10);
        assert_eq!(metrics.messages_sent(), 10);
        assert_eq!(metrics.messages_received(), 10);
    }

    #[test]
    fn test_byte_accumulation() {
        let metrics = WebSocketMetrics::new();
        metrics.record_message_sent(100);
        metrics.record_message_sent(50);
        metrics.record_message_received(75);
        
        assert_eq!(metrics.bytes_sent(), 150);
        assert_eq!(metrics.bytes_received(), 75);
    }

    #[test]
    fn test_error_accumulation() {
        let metrics = WebSocketMetrics::new();
        metrics.record_connection_error();
        metrics.record_connection_error();
        metrics.record_message_error();
        
        assert_eq!(metrics.connection_errors(), 2);
        assert_eq!(metrics.message_errors(), 1);
    }

    #[test]
    fn test_default_metrics() {
        let metrics = WebSocketMetrics::default();
        assert_eq!(metrics.total_connections(), 0);
        assert_eq!(metrics.active_connections(), 0);
    }
}
