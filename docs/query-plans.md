# Automated Query Plan Capture & Incident Investigation Guide

## Overview

The Slow Query Plan Capture engine in `synapse-core` (`src/db/slow_query.rs`) automatically captures and stores `EXPLAIN ANALYZE` execution plans whenever database queries exceed a configured latency threshold (default: 100ms).

Capturing the exact query plan at the moment of execution preserves empirical performance evidence. This eliminates the need to manually reproduce queries after data distribution or table statistics have changed.

---

## Architecture & How It Works

1. **Threshold Monitoring**: Queries executed via `PoolManager` or `SlowQueryLogger` are timed against `SlowQueryConfig::threshold_ms`.
2. **Safety Guards**: 
   - **Read Queries (`SELECT`)**: Captures `EXPLAIN ANALYZE` execution plans via application-level execution or PostgreSQL server logs.
   - **Write Queries (`INSERT`, `UPDATE`, `DELETE`, etc.)**: Bypasses application-level re-execution to prevent side-effects, duplicate writes, or lock contention.
3. **Payload Truncation**: Plans exceeding `max_plan_bytes` (default: 64 KB) are safely truncated to prevent unbounded memory growth.
4. **Bounded Storage Retention**: Captured query records are stored in a thread-safe ring buffer (`VecDeque`) capped at `max_stored_queries` (default: 100). Oldest entries are automatically evicted when full.

---

## Configuration

### Application Settings (`SlowQueryConfig`)

```rust
use synapse_core::db::slow_query::{SlowQueryConfig, SlowQueryLogger};

let config = SlowQueryConfig {
    threshold_ms: 100,         // Log and capture plans for queries >= 100ms
    max_stored_queries: 100,   // Maximum ring buffer capacity
    max_plan_bytes: 65536,     // Maximum plan payload size (64 KB)
    auto_explain_enabled: true,// Enable auto_explain extension settings
    capture_explain: true,     // Enable EXPLAIN ANALYZE capture
};

let logger = SlowQueryLogger::new(config);
```

### PostgreSQL `auto_explain` Extension Setup

PostgreSQL provides the native `auto_explain` extension to log execution plans directly into server logs for all queries (including write operations) without application re-execution overhead.

#### `postgresql.conf` Configuration

```ini
# Add auto_explain to shared preload libraries
shared_preload_libraries = 'auto_explain'

# Configure auto_explain parameters
auto_explain.log_min_duration = '100ms'
auto_explain.log_analyze = true
auto_explain.log_verbose = true
auto_explain.log_buffers = true
auto_explain.log_format = text
auto_explain.log_nested_statements = true
```

#### Session-Level Configuration

You can enable `auto_explain` on a specific connection pool session:

```rust
logger.configure_auto_explain_session(pool_manager.primary()).await?;
```

---

## Safety & Performance Overhead

| Security & Safety Principle | Mechanism |
| :--- | :--- |
| **Write Query Safety** | `is_write_query()` inspects SQL prefixes (`INSERT`, `UPDATE`, `DELETE`, `CREATE`, etc.) and bypasses re-execution. |
| **Zero Latency Amplification** | Fast queries (`< threshold_ms`) bypass plan capture entirely. |
| **Bounded Memory Footprint** | Ring buffer capacity (`max_stored_queries`) and payload truncation (`max_plan_bytes`) eliminate memory leak risks. |

---

## Incident Response: Querying Captured Plans

When investigating a production incident or latency spike, query captured plans directly from `SlowQueryLogger`:

### 1. Retrieve All Captured Slow Queries

```rust
let slow_queries = logger.get_slow_queries().await;
for record in slow_queries {
    println!("Query ID: {}", record.id);
    println!("Timestamp: {}", record.timestamp);
    println!("Duration: {} ms", record.duration_ms);
    println!("Query: {}", record.query_text);
    if let Some(plan) = record.explain_plan {
        println!("EXPLAIN Plan:\n{}", plan);
    }
}
```

### 2. Search Plans for a Specific Query Substring

```rust
// Find captured plans for settlement queries
let settlement_plans = logger.get_plans_for_query("settlements").await;
for rec in settlement_plans {
    println!("Found slow settlement query ({}ms):\n{}", rec.duration_ms, rec.explain_plan.unwrap_or_default());
}
```

### 3. Log Output Format

Slow queries are simultaneously logged via `tracing::warn`:

```json
{
  "timestamp": "2026-08-28T07:55:00Z",
  "level": "WARN",
  "fields": {
    "duration_ms": 245,
    "threshold_ms": 100,
    "is_write": false,
    "message": "Slow query detected: SELECT * FROM transactions WHERE status = 'pending'"
  }
}
```
