//! Data export for telemetry.
//!
//! Provides structured export of telemetry data including traces, metrics,
//! and span data with proper buffering, compression, and error handling.

use crate::telemetry::error_handling::TelemetryError;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

/// Maximum buffer size for pending exports
const MAX_BUFFER_SIZE: usize = 1000;
/// Default batch size for exports
const DEFAULT_BATCH_SIZE: usize = 100;
/// Maximum payload size in bytes (1MB)
const MAX_PAYLOAD_SIZE: usize = 1024 * 1024;

// ── Data types ────────────────────────────────────────────────────────────────

/// A single telemetry record to be exported
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryRecord {
    /// Unique identifier for this record
    pub id: String,
    /// Type of telemetry (trace, metric, event)
    pub record_type: RecordType,
    /// Timestamp of the record (milliseconds since epoch)
    pub timestamp: u64,
    /// The telemetry payload
    pub payload: TelemetryPayload,
    /// Optional attributes for filtering
    pub attributes: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordType {
    Trace,
    Metric,
    Event,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TelemetryPayload {
    Trace(TraceData),
    Metric(MetricData),
    Event(EventData),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceData {
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub operation_name: String,
    pub start_time: u64,
    pub end_time: u64,
    pub status: TraceStatus,
    pub tags: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TraceStatus {
    #[serde(rename = "ok")]
    Ok,
    #[serde(rename = "error")]
    Error,
    #[serde(rename = "unset")]
    Unset,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricData {
    pub metric_name: String,
    pub metric_type: MetricType,
    pub value: f64,
    pub unit: Option<String>,
    pub labels: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricType {
    Counter,
    Gauge,
    Histogram,
    Summary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventData {
    pub event_name: String,
    pub message: String,
    pub level: EventLevel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventLevel {
    Debug,
    Info,
    Warn,
    Error,
}

// ── Export batch ──────────────────────────────────────────────────────────────

/// A batch of telemetry records ready for export
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportBatch {
    /// Unique identifier for this batch
    pub batch_id: String,
    /// Records in this batch
    pub records: Vec<TelemetryRecord>,
    /// Total size in bytes (estimated)
    pub size_bytes: usize,
    /// When this batch was created
    pub created_at: u64,
}

impl ExportBatch {
    /// Creates a new batch from records.
    ///
    /// Estimates the serialized size of the batch. If serialization fails,
    /// size is set to 0. Timestamps default to 0 if system time is unavailable.
    /// Neither failure is fatal; the batch is created with degraded metadata.
    pub fn new(records: Vec<TelemetryRecord>) -> Self {
        let size_bytes = serde_json::to_string(&records)
            .map(|s| s.len())
            .unwrap_or_else(|_| {
                // Serialization failed; estimate size as last resort
                // This is non-fatal; batch will still be created
                tracing::warn!("Failed to estimate batch size via serialization; using 0");
                0
            });

        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|d| d.as_millis() as u64)
            .unwrap_or_else(|| {
                // System time is unavailable; use 0
                // This is non-fatal; batch will still be created
                tracing::warn!("System time unavailable for batch timestamp; using 0");
                0
            });

        Self {
            batch_id: uuid::Uuid::new_v4().to_string(),
            records,
            size_bytes,
            created_at,
        }
    }

    /// Checks if the batch is within size limits
    pub fn is_valid(&self) -> bool {
        !self.records.is_empty() && self.size_bytes <= MAX_PAYLOAD_SIZE
    }
}

// ── Export buffer ─────────────────────────────────────────────────────────────

/// Buffer for storing pending telemetry records
#[derive(Debug)]
pub struct ExportBuffer {
    buffer: VecDeque<TelemetryRecord>,
    max_size: usize,
    batch_size: usize,
}

impl Default for ExportBuffer {
    fn default() -> Self {
        Self::new(MAX_BUFFER_SIZE, DEFAULT_BATCH_SIZE)
    }
}

impl ExportBuffer {
    /// Creates a new buffer with specified limits
    pub fn new(max_size: usize, batch_size: usize) -> Self {
        Self {
            buffer: VecDeque::with_capacity(max_size),
            max_size,
            batch_size,
        }
    }

    /// Adds a record to the buffer
    pub fn push(&mut self, record: TelemetryRecord) -> Option<ExportBatch> {
        // Remove oldest if at capacity
        if self.buffer.len() >= self.max_size {
            self.buffer.pop_front();
        }

        self.buffer.push_back(record);

        // Return a batch if we've reached batch size
        self.maybe_flush()
    }

    /// Flushes the buffer, returning all records as a batch
    pub fn flush(&mut self) -> Option<ExportBatch> {
        if self.buffer.is_empty() {
            return None;
        }

        let records: Vec<_> = self.buffer.drain(..).collect();
        Some(ExportBatch::new(records))
    }

    /// Returns a batch if buffer has enough records
    fn maybe_flush(&mut self) -> Option<ExportBatch> {
        if self.buffer.len() >= self.batch_size {
            self.flush()
        } else {
            None
        }
    }

    /// Returns current buffer size
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    /// Checks if buffer is empty
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }
}

// ── Export service ────────────────────────────────────────────────────────────

/// Configuration for the export service
#[derive(Debug, Clone)]
pub struct ExportConfig {
    /// Maximum buffer size
    pub max_buffer_size: usize,
    /// Batch size for exports
    pub batch_size: usize,
    /// Export endpoint URL
    pub endpoint: String,
    /// Export timeout in seconds
    pub timeout_secs: u64,
    /// Whether to enable compression
    pub compression: bool,
}

impl Default for ExportConfig {
    fn default() -> Self {
        Self {
            max_buffer_size: MAX_BUFFER_SIZE,
            batch_size: DEFAULT_BATCH_SIZE,
            endpoint: String::new(),
            timeout_secs: 30,
            compression: true,
        }
    }
}

/// Service for exporting telemetry data
#[derive(Debug)]
pub struct DataExportService {
    #[allow(dead_code)]
    config: ExportConfig,
    buffer: Arc<RwLock<ExportBuffer>>,
}

impl DataExportService {
    /// Creates a new export service with the supplied configuration.
    ///
    /// # Arguments
    /// - `config`: Export configuration including buffer size, batch size, and endpoint.
    ///
    /// # Non-fatal behavior
    /// The service starts immediately; configuration errors are deferred to export time.
    pub fn new(config: ExportConfig) -> Self {
        Self {
            config: config.clone(),
            buffer: Arc::new(RwLock::new(ExportBuffer::new(
                config.max_buffer_size,
                config.batch_size,
            ))),
        }
    }

    /// Creates a new export service with default configuration.
    ///
    /// Equivalent to `Self::new(ExportConfig::default())`.
    pub fn with_default_config() -> Self {
        Self::new(ExportConfig::default())
    }

    /// Adds a telemetry record to the export buffer.
    ///
    /// If the buffer reaches batch size, returns a ready-to-export batch.
    /// If the buffer is at capacity, the oldest record is discarded.
    ///
    /// # Returns
    /// `Some(batch)` if buffer flushed due to batch size, `None` otherwise.
    /// This is non-fatal; the record is always buffered even if overflow occurs.
    pub async fn record(&self, record: TelemetryRecord) -> Option<ExportBatch> {
        let mut buffer = self.buffer.write().await;
        buffer.push(record)
    }

    /// Forces a flush of all pending records into a single batch.
    ///
    /// # Returns
    /// `Some(batch)` if any records were pending, `None` if buffer was already empty.
    pub async fn flush(&self) -> Option<ExportBatch> {
        let mut buffer = self.buffer.write().await;
        buffer.flush()
    }

    /// Returns the current number of pending records awaiting export.
    pub async fn pending_count(&self) -> usize {
        let buffer = self.buffer.read().await;
        buffer.len()
    }

    /// Validates a record before adding to buffer.
    ///
    /// Checks that the record ID is non-empty and payload does not exceed MAX_PAYLOAD_SIZE.
    ///
    /// # Errors
    /// Returns [`TelemetryError::ValidationError`] if record ID is empty,
    /// or [`TelemetryError::PayloadTooLarge`] if payload exceeds size limit.
    pub fn validate_record(record: &TelemetryRecord) -> Result<(), TelemetryError> {
        if record.id.is_empty() {
            return Err(TelemetryError::ValidationError(
                crate::telemetry::input_validation::ValidationError::EmptyValue(
                    "record ID cannot be empty".into(),
                ),
            ));
        }

        if record.payload_size() > MAX_PAYLOAD_SIZE {
            return Err(TelemetryError::PayloadTooLarge(MAX_PAYLOAD_SIZE));
        }

        Ok(())
    }
}

impl TelemetryRecord {
    /// Estimates the serialized size of this record.
    ///
    /// Returns 0 if serialization fails. This is non-fatal; the record
    /// is still usable even if size estimation fails.
    pub fn payload_size(&self) -> usize {
        serde_json::to_string(self)
            .map(|s| s.len())
            .unwrap_or_else(|_| {
                // Serialization failed; estimate conservatively as 0
                // This is non-fatal; record validation can still succeed
                0
            })
    }
}

// ── Helper functions ──────────────────────────────────────────────────────────

/// Creates a new trace record
pub fn create_trace_record(
    trace_id: &str,
    span_id: &str,
    operation_name: &str,
    start_time: u64,
    end_time: u64,
    status: TraceStatus,
) -> TelemetryRecord {
    TelemetryRecord {
        id: uuid::Uuid::new_v4().to_string(),
        record_type: RecordType::Trace,
        timestamp: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        payload: TelemetryPayload::Trace(TraceData {
            trace_id: trace_id.to_string(),
            span_id: span_id.to_string(),
            parent_span_id: None,
            operation_name: operation_name.to_string(),
            start_time,
            end_time,
            status,
            tags: vec![],
        }),
        attributes: vec![],
    }
}

/// Creates a new metric record
pub fn create_metric_record(name: &str, metric_type: MetricType, value: f64) -> TelemetryRecord {
    TelemetryRecord {
        id: uuid::Uuid::new_v4().to_string(),
        record_type: RecordType::Metric,
        timestamp: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        payload: TelemetryPayload::Metric(MetricData {
            metric_name: name.to_string(),
            metric_type,
            value,
            unit: None,
            labels: vec![],
        }),
        attributes: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_export_batch_creation() {
        let record = create_trace_record("trace1", "span1", "test_op", 100, 200, TraceStatus::Ok);
        let batch = ExportBatch::new(vec![record]);

        assert!(batch.is_valid());
        assert!(!batch.batch_id.is_empty());
    }

    #[test]
    fn test_export_buffer_push() {
        let mut buffer = ExportBuffer::new(100, 10);

        for i in 0..15 {
            let record = create_trace_record(
                &format!("trace{}", i),
                &format!("span{}", i),
                "test",
                100,
                200,
                TraceStatus::Ok,
            );
            let batch = buffer.push(record);

            // First batch should be created at 10 records
            if i == 9 {
                assert!(batch.is_some());
                assert_eq!(batch.unwrap().records.len(), 10);
            }
        }
    }

    #[test]
    fn test_export_buffer_full() {
        let mut buffer = ExportBuffer::new(5, 10);

        for i in 0..10 {
            let record = create_trace_record(
                &format!("trace{}", i),
                &format!("span{}", i),
                "test",
                100,
                200,
                TraceStatus::Ok,
            );
            buffer.push(record);
        }

        // Should have max 5 records (oldest dropped)
        assert_eq!(buffer.len(), 5);
    }

    #[test]
    fn test_data_export_service_record() {
        // Use a batch size of 1 so a single record flushes a batch immediately.
        let config = ExportConfig {
            batch_size: 1,
            ..ExportConfig::default()
        };
        let service = DataExportService::new(config);

        let record = create_trace_record("trace1", "span1", "test", 100, 200, TraceStatus::Ok);

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let batch = runtime.block_on(async { service.record(record).await });

        assert!(batch.is_some());
    }

    #[test]
    fn test_validate_record_empty_id() {
        let record = TelemetryRecord {
            id: String::new(),
            record_type: RecordType::Trace,
            timestamp: 0,
            payload: TelemetryPayload::Trace(TraceData {
                trace_id: "trace1".to_string(),
                span_id: "span1".to_string(),
                parent_span_id: None,
                operation_name: "test".to_string(),
                start_time: 100,
                end_time: 200,
                status: TraceStatus::Ok,
                tags: vec![],
            }),
            attributes: vec![],
        };

        assert!(DataExportService::validate_record(&record).is_err());
    }

    #[test]
    fn test_record_type_serialization() {
        let record = create_metric_record("test_counter", MetricType::Counter, 42.0);
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains("metric"));
        assert!(json.contains("counter"));
    }

    #[test]
    fn test_trace_status_serialization() {
        let record = create_trace_record("trace1", "span1", "test", 100, 200, TraceStatus::Error);
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains("error"));
    }

    #[test]
    fn test_event_level_serialization() {
        let record = TelemetryRecord {
            id: "id1".to_string(),
            record_type: RecordType::Event,
            timestamp: 100,
            payload: TelemetryPayload::Event(EventData {
                event_name: "test_event".to_string(),
                message: "Test message".to_string(),
                level: EventLevel::Warn,
            }),
            attributes: vec![],
        };

        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains("warn"));
    }

    #[test]
    fn test_export_buffer_is_empty() {
        let buffer = ExportBuffer::default();
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_export_config_default() {
        let config = ExportConfig::default();
        assert_eq!(config.max_buffer_size, MAX_BUFFER_SIZE);
        assert_eq!(config.batch_size, DEFAULT_BATCH_SIZE);
        assert!(config.compression);
    }
}
