//! Historical market data recorder and quote archive (#1508).
//!
//! * **Quotes** (full fidelity, durable): `record_quote` inserts synchronously
//!   into `swap_quote_archive`; a loss here is not acceptable.
//! * **Price snapshots** (sampled): `SnapshotRecorder` is write-behind with a
//!   bounded buffer. A full buffer drops the sample (never blocks quoting) and a
//!   hard crash may lose the buffered tail.
//!
//! Tables are monthly range partitions created ahead of time by
//! `ensure_swap_archive_partitions`; partitions older than the retention window
//! are detached and queued for export via [`ArchiveExporter`].

use crate::alerting::{names, AlertPayload, AlertSeverity};
use async_trait::async_trait;
use bigdecimal::BigDecimal;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, QueryBuilder};
use tokio::sync::mpsc;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct QuoteRecord {
    pub quote_id: Uuid,
    pub order_id: Option<Uuid>,
    pub tenant_id: Uuid,
    pub pair: String,
    pub inputs: serde_json::Value,
    pub route: serde_json::Value,
    pub output: serde_json::Value,
    pub quoted_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct PriceSnapshot {
    pub pair: String,
    pub source: String,
    pub bid: BigDecimal,
    pub ask: BigDecimal,
    pub mid: BigDecimal,
    pub captured_at: DateTime<Utc>,
}

/// Archive settings (`SWAP_ARCHIVE_*`). Retention follows the swap retention
/// policy; snapshots are sampled at `snapshot_interval_secs` per pair.
#[derive(Debug, Clone)]
pub struct ArchiveConfig {
    pub retention_months: i32,
    pub partitions_ahead: i32,
    pub snapshot_interval_secs: u64,
    pub buffer_capacity: usize,
    pub batch_size: usize,
}

impl Default for ArchiveConfig {
    fn default() -> Self {
        Self {
            retention_months: 13,
            partitions_ahead: 3,
            snapshot_interval_secs: 5,
            buffer_capacity: 10_000,
            batch_size: 500,
        }
    }
}

impl ArchiveConfig {
    pub fn from_env() -> Self {
        fn env<T: std::str::FromStr>(k: &str, d: T) -> T {
            std::env::var(k)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(d)
        }
        let d = Self::default();
        Self {
            retention_months: env("SWAP_ARCHIVE_RETENTION_MONTHS", d.retention_months),
            partitions_ahead: env("SWAP_ARCHIVE_PARTITIONS_AHEAD", d.partitions_ahead),
            snapshot_interval_secs: env(
                "SWAP_ARCHIVE_SNAPSHOT_INTERVAL_SECS",
                d.snapshot_interval_secs,
            ),
            buffer_capacity: env("SWAP_ARCHIVE_BUFFER_CAPACITY", d.buffer_capacity),
            batch_size: env("SWAP_ARCHIVE_BATCH_SIZE", d.batch_size),
        }
    }
}

/// Durable quote write; call after the quote is issued.
pub async fn record_quote(pool: &PgPool, q: &QuoteRecord) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO swap_quote_archive (quote_id, order_id, tenant_id, pair, inputs, route, output, quoted_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
    )
    .bind(q.quote_id)
    .bind(q.order_id)
    .bind(q.tenant_id)
    .bind(&q.pair)
    .bind(&q.inputs)
    .bind(&q.route)
    .bind(&q.output)
    .bind(q.quoted_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// Batched insert of snapshots (one statement per call).
pub async fn insert_snapshots(pool: &PgPool, batch: &[PriceSnapshot]) -> Result<u64, sqlx::Error> {
    if batch.is_empty() {
        return Ok(0);
    }
    let mut qb = QueryBuilder::new(
        "INSERT INTO swap_price_snapshots (pair, source, bid, ask, mid, captured_at) ",
    );
    qb.push_values(batch, |mut b, s| {
        b.push_bind(&s.pair)
            .push_bind(&s.source)
            .push_bind(&s.bid)
            .push_bind(&s.ask)
            .push_bind(&s.mid)
            .push_bind(s.captured_at);
    });
    Ok(qb.build().execute(pool).await?.rows_affected())
}

/// Non-blocking handle for the write-behind snapshot buffer.
#[derive(Clone)]
pub struct SnapshotRecorder {
    tx: mpsc::Sender<PriceSnapshot>,
}

impl SnapshotRecorder {
    /// Spawns the flusher. Returns immediately; `record` never blocks.
    pub fn spawn(pool: PgPool, cfg: &ArchiveConfig) -> Self {
        let (tx, mut rx) = mpsc::channel::<PriceSnapshot>(cfg.buffer_capacity);
        let batch_size = cfg.batch_size;
        tokio::spawn(async move {
            let mut batch = Vec::with_capacity(batch_size);
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
            loop {
                tokio::select! {
                    maybe = rx.recv() => match maybe {
                        Some(s) => { batch.push(s); if batch.len() < batch_size { continue; } }
                        None => { let _ = insert_snapshots(&pool, &batch).await; return; }
                    },
                    _ = tick.tick() => {}
                }
                if let Err(e) = insert_snapshots(&pool, &batch).await {
                    // e.g. disk full: degrade, alert, and drop the batch rather than stall.
                    AlertPayload::new(
                        names::SWAP_ARCHIVE_DEGRADED,
                        AlertSeverity::Warning,
                        format!("snapshot flush failed: {e}"),
                    )
                    .with_label("dropped", batch.len())
                    .emit();
                }
                batch.clear();
            }
        });
        Self { tx }
    }

    /// Queues a snapshot; returns `false` (and drops it) if the buffer is full.
    pub fn record(&self, s: PriceSnapshot) -> bool {
        self.tx.try_send(s).is_ok()
    }
}

/// Ships a detached partition to long-term object storage.
#[async_trait]
pub trait ArchiveExporter: Send + Sync {
    async fn export_partition(&self, pool: &PgPool, partition: &str) -> Result<(), String>;
}

/// One maintenance pass: create upcoming partitions, detach expired ones,
/// then export + drop detached partitions. Export failures alert and retry
/// next pass; they never affect quoting.
pub async fn maintain(
    pool: &PgPool,
    cfg: &ArchiveConfig,
    exporter: Option<&dyn ArchiveExporter>,
) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT ensure_swap_archive_partitions($1)")
        .bind(cfg.partitions_ahead)
        .execute(pool)
        .await?;
    sqlx::query("SELECT detach_swap_archive_partitions($1)")
        .bind(cfg.retention_months)
        .execute(pool)
        .await?;
    let Some(exporter) = exporter else {
        return Ok(());
    };
    let pending: Vec<(String,)> = sqlx::query_as(
        "SELECT partition_name FROM swap_archive_exports WHERE status <> 'exported'",
    )
    .fetch_all(pool)
    .await?;
    for (part,) in pending {
        // `part` comes from pg_class via our own function; quote it as an identifier anyway.
        match exporter.export_partition(pool, &part).await {
            Ok(()) => {
                sqlx::query(&format!(
                    "DROP TABLE IF EXISTS \"{}\"",
                    part.replace('"', "\"\"")
                ))
                .execute(pool)
                .await?;
                sqlx::query("UPDATE swap_archive_exports SET status='exported', exported_at=now(), last_error=NULL WHERE partition_name=$1")
                    .bind(&part)
                    .execute(pool)
                    .await?;
            }
            Err(e) => {
                sqlx::query("UPDATE swap_archive_exports SET status='failed', last_error=$2 WHERE partition_name=$1")
                    .bind(&part)
                    .bind(&e)
                    .execute(pool)
                    .await?;
                AlertPayload::new(
                    names::SWAP_ARCHIVE_DEGRADED,
                    AlertSeverity::Warning,
                    format!("archive export of {part} failed: {e}"),
                )
                .with_label("partition", &part)
                .emit();
            }
        }
    }
    Ok(())
}

/// Background maintenance; opt-in via `SWAP_ARCHIVE_ENABLED=true`.
pub fn spawn_maintenance(pool: PgPool, exporter: Option<std::sync::Arc<dyn ArchiveExporter>>) {
    if std::env::var("SWAP_ARCHIVE_ENABLED").ok().as_deref() != Some("true") {
        return;
    }
    tokio::spawn(async move {
        let cfg = ArchiveConfig::from_env();
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
        loop {
            tick.tick().await;
            if let Err(e) = maintain(&pool, &cfg, exporter.as_deref()).await {
                AlertPayload::new(
                    names::SWAP_ARCHIVE_DEGRADED,
                    AlertSeverity::Warning,
                    format!("archive maintenance failed: {e}"),
                )
                .emit();
            }
        }
    });
}

#[derive(Debug, Default, Deserialize)]
pub struct QuoteQuery {
    pub quote_id: Option<Uuid>,
    pub order_id: Option<Uuid>,
    pub tenant_id: Option<Uuid>,
    pub pair: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub limit: Option<i64>,
}

/// Retrieves archived quotes by quote/order ID, tenant, pair and time range.
pub async fn query_quotes(pool: &PgPool, q: &QuoteQuery) -> Result<Vec<QuoteRecord>, sqlx::Error> {
    sqlx::query_as(
        "SELECT quote_id, order_id, tenant_id, pair, inputs, route, output, quoted_at
         FROM swap_quote_archive
         WHERE ($1::uuid IS NULL OR quote_id = $1) AND ($2::uuid IS NULL OR order_id = $2)
           AND ($3::uuid IS NULL OR tenant_id = $3) AND ($4::text IS NULL OR pair = $4)
           AND ($5::timestamptz IS NULL OR quoted_at >= $5) AND ($6::timestamptz IS NULL OR quoted_at < $6)
         ORDER BY quoted_at DESC LIMIT $7",
    )
    .bind(q.quote_id)
    .bind(q.order_id)
    .bind(q.tenant_id)
    .bind(&q.pair)
    .bind(q.from)
    .bind(q.to)
    .bind(q.limit.unwrap_or(100).clamp(1, 1000))
    .fetch_all(pool)
    .await
}

#[derive(Debug, Deserialize)]
pub struct PriceQuery {
    pub pair: String,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub limit: Option<i64>,
}

pub async fn query_prices(
    pool: &PgPool,
    q: &PriceQuery,
) -> Result<Vec<PriceSnapshot>, sqlx::Error> {
    sqlx::query_as(
        "SELECT pair, source, bid, ask, mid, captured_at FROM swap_price_snapshots
         WHERE pair = $1 AND captured_at >= $2 AND captured_at < $3 ORDER BY captured_at LIMIT $4",
    )
    .bind(&q.pair)
    .bind(q.from)
    .bind(q.to)
    .bind(q.limit.unwrap_or(1000).clamp(1, 10_000))
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap() -> PriceSnapshot {
        PriceSnapshot {
            pair: "USDC/XLM".into(),
            source: "aggregate".into(),
            bid: BigDecimal::from(1),
            ask: BigDecimal::from(2),
            mid: BigDecimal::from(1),
            captured_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn recorder_drops_instead_of_blocking_when_buffer_is_full() {
        // A lazy pool never connects; the flusher fails and alerts, but the
        // producer-side contract under test is that `record` never blocks.
        let pool = PgPool::connect_lazy("postgres://localhost/none").unwrap();
        let cfg = ArchiveConfig {
            buffer_capacity: 1,
            batch_size: 1000,
            ..Default::default()
        };
        let (tx, _rx) = mpsc::channel::<PriceSnapshot>(cfg.buffer_capacity);
        let rec = SnapshotRecorder { tx };
        assert!(rec.record(snap()));
        assert!(!rec.record(snap()), "full buffer must drop, not block");
        drop(pool);
    }

    #[test]
    fn defaults_define_retention_and_sampling() {
        let c = ArchiveConfig::default();
        assert_eq!(c.retention_months, 13);
        assert!(c.snapshot_interval_secs > 0 && c.buffer_capacity > 0);
    }
}
