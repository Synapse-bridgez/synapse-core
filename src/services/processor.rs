use sqlx::PgPool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::watch;
use tokio::time::{sleep, Duration};
use tracing::{debug, error, info, warn};

use crate::db::models::Transaction;
use crate::services::lock_manager::LeaderElection;
use crate::stellar::HorizonClient;

const LEADER_HEARTBEAT_SECS: u64 = 15;
const POLL_INTERVAL_SECS: u64 = 5;

/// Exponential moving average tracker for adaptive batch sizing.
pub struct BatchSizer {
    ema: f64,
    alpha: f64,
    min_batch: u32,
    max_batch: u32,
    scaling_factor: f64,
}

impl BatchSizer {
    pub fn new(min_batch: u32, max_batch: u32, scaling_factor: f64) -> Self {
        Self {
            ema: min_batch as f64,
            alpha: 0.2, // EMA smoothing factor
            min_batch,
            max_batch,
            scaling_factor,
        }
    }

    /// Update EMA with the latest queue depth and return the new batch size.
    pub fn update(&mut self, queue_depth: u64) -> u32 {
        self.ema = self.alpha * queue_depth as f64 + (1.0 - self.alpha) * self.ema;
        let raw = (self.ema * self.scaling_factor).round() as u32;
        raw.clamp(self.min_batch, self.max_batch)
    }

    pub fn current(&self) -> u32 {
        let raw = (self.ema * self.scaling_factor).round() as u32;
        raw.clamp(self.min_batch, self.max_batch)
    }
}

pub struct ProcessorPool {
    pool: PgPool,
    horizon_client: HorizonClient,
    workers: usize,
    poll_interval_ms: u64,
    min_batch: u32,
    max_batch: u32,
    scaling_factor: f64,
    /// Shared atomic for current batch size (exposed via /health).
    current_batch_size: Arc<AtomicU64>,
    /// Shared atomic for queue depth (read by back-pressure task).
    pending_queue_depth: Arc<AtomicU64>,
}

impl ProcessorPool {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool: PgPool,
        horizon_client: HorizonClient,
        workers: usize,
        poll_interval_ms: u64,
        min_batch: u32,
        max_batch: u32,
        scaling_factor: f64,
        current_batch_size: Arc<AtomicU64>,
        pending_queue_depth: Arc<AtomicU64>,
    ) -> Self {
        Self {
            pool,
            horizon_client,
            workers,
            poll_interval_ms,
            min_batch,
            max_batch,
            scaling_factor,
            current_batch_size,
            pending_queue_depth,
        }
    }

    /// Start the processor pool. Returns a shutdown sender; drop or send to it to stop workers.
    pub fn start(self) -> watch::Sender<bool> {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let workers = self.workers;
        let poll_interval_ms = self.poll_interval_ms;
        let min_batch = self.min_batch;
        let max_batch = self.max_batch;
        let scaling_factor = self.scaling_factor;
        let current_batch_size = self.current_batch_size.clone();
        let pending_queue_depth = self.pending_queue_depth.clone();
        let pool = self.pool;
        let horizon_client = self.horizon_client;

        info!("Starting ProcessorPool with {} workers", workers);

        for worker_id in 0..workers {
            let pool = pool.clone();
            let horizon_client = horizon_client.clone();
            let mut shutdown_rx = shutdown_rx.clone();
            let current_batch_size = current_batch_size.clone();
            let pending_queue_depth = pending_queue_depth.clone();
            let mut sizer = BatchSizer::new(min_batch, max_batch, scaling_factor);

            tokio::spawn(async move {
                info!("Processor worker {} started", worker_id);
                loop {
                    // Check for shutdown signal
                    if *shutdown_rx.borrow() {
                        info!("Processor worker {} shutting down", worker_id);
                        break;
                    }

                    let depth = pending_queue_depth.load(Ordering::Relaxed);
                    let batch_size = sizer.update(depth);
                    current_batch_size.store(batch_size as u64, Ordering::Relaxed);
                    debug!(worker_id, batch_size, depth, "adaptive batch size");

                    match process_batch(&pool, &horizon_client, batch_size).await {
                        Ok(processed) => {
                            if processed > 0 {
                                tracing::info!(
                                    counter.processor_transactions_processed = processed as u64,
                                    worker_id,
                                    "processed transactions"
                                );
                            }
                            tracing::info!(counter.processor_batches_total = 1u64, worker_id);
                        }
                        Err(e) => {
                            error!(worker_id, "Processor batch error: {}", e);
                        }
                    }

                    // Wait for poll interval or shutdown
                    tokio::select! {
                        _ = sleep(Duration::from_millis(poll_interval_ms)) => {}
                        _ = shutdown_rx.changed() => {
                            info!("Processor worker {} received shutdown signal", worker_id);
                            break;
                        }
                    }
                }
                info!("Processor worker {} stopped", worker_id);
            });
        }

        shutdown_tx
    }
}

pub async fn process_batch(
    pool: &PgPool,
    _horizon_client: &HorizonClient,
    batch_size: u32,
) -> anyhow::Result<usize> {
    let mut tx = pool.begin().await?;

    let pending: Vec<Transaction> = sqlx::query_as::<_, Transaction>(
        r#"
        SELECT id, stellar_account, amount, asset_code, status, created_at, updated_at,
               anchor_transaction_id, callback_type, callback_status, settlement_id,
               memo, memo_type, metadata, priority
        FROM transactions
        WHERE status = 'pending'
        ORDER BY created_at ASC
        LIMIT $1
        FOR UPDATE SKIP LOCKED
        "#,
    )
    .bind(batch_size as i64)
    .fetch_all(&mut *tx)
    .await?;

    if pending.is_empty() {
        tx.commit().await?;
        return Ok(0);
    }

    debug!("Processing {} pending transaction(s)", pending.len());

    let count = pending.len();
    let mut asset_codes = std::collections::HashSet::new();
    for transaction in &pending {
        asset_codes.insert(transaction.asset_code.clone());
    }

    // TODO: per-transaction processing logic
    for _transaction in pending {
        // process each transaction
    }

    tx.commit().await?;

    for asset_code in asset_codes {
        crate::db::queries::invalidate_caches_for_asset(&asset_code).await;
    }

    Ok(count)
}

/// Legacy single-worker entry point kept for backward compatibility.
pub async fn run_processor(pool: PgPool, horizon_client: HorizonClient) {
    info!("Async transaction processor started (legacy single-worker)");
    loop {
        if let Err(e) = process_batch(&pool, &horizon_client, 10).await {
            error!("Processor batch error: {}", e);
        }
        sleep(Duration::from_secs(5)).await;
    }
}

/// Background task: refresh pending queue depth every 5 seconds.
pub async fn queue_depth_task(pool: PgPool, pending_queue_depth: Arc<AtomicU64>) {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    loop {
        interval.tick().await;
        match sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM transactions WHERE status = 'pending'",
        )
        .fetch_one(&pool)
        .await
        {
            Ok(count) => {
                let depth = count.max(0) as u64;
                pending_queue_depth.store(depth, Ordering::Relaxed);
                tracing::info!(counter.processor_queue_depth = depth);
                if depth > 5_000 {
                    warn!(depth, "Pending transaction queue depth is high");
                }
            }
            Err(e) => {
                error!("Failed to query pending queue depth: {}", e);
                // Fail open: leave the existing counter unchanged
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_sizer_clamps_to_min() {
        let mut s = BatchSizer::new(10, 500, 0.5);
        let size = s.update(0);
        assert!(size >= 10);
    }

    #[test]
    fn batch_sizer_clamps_to_max() {
        let mut s = BatchSizer::new(10, 500, 0.5);
        // Feed a very large depth many times to push EMA up
        for _ in 0..50 {
            s.update(100_000);
        }
        let size = s.current();
        assert!(size <= 500);
    }

    #[test]
    fn batch_sizer_increases_under_load() {
        let mut s = BatchSizer::new(10, 500, 0.5);
        let initial = s.current();
        for _ in 0..20 {
            s.update(1_000);
        }
        assert!(s.current() > initial);
    }

    #[test]
    fn batch_sizer_decreases_during_idle() {
        let mut s = BatchSizer::new(10, 500, 0.5);
        // Prime with high load
        for _ in 0..20 {
            s.update(1_000);
        }
        let high = s.current();
        // Then idle
        for _ in 0..50 {
            s.update(0);
        }
        assert!(s.current() < high);
    }
}

/// Runs the leader election + heartbeat loop.
///
/// - All instances call this; only the elected leader returns `true` from
///   `try_acquire_leadership`.
/// - The leader runs partition maintenance, settlement jobs, and webhook dispatch.
/// - All instances run `process_batch` (safe via SKIP LOCKED).
pub async fn run_processor_with_leader_election(
    pool: PgPool,
    horizon_client: HorizonClient,
    redis_url: &str,
) {
    let election = match LeaderElection::new(redis_url) {
        Ok(e) => e,
        Err(e) => {
            warn!("Failed to create LeaderElection (Redis unavailable?): {e}. Running without leader guard.");
            run_processor(pool, horizon_client).await;
            return;
        }
    };

    info!(
        instance_id = election.instance_id(),
        "Processor started with leader election"
    );

    let mut heartbeat_tick = tokio::time::interval(Duration::from_secs(LEADER_HEARTBEAT_SECS));
    let mut process_tick = tokio::time::interval(Duration::from_secs(POLL_INTERVAL_SECS));

    loop {
        tokio::select! {
            _ = heartbeat_tick.tick() => {
                // Publish heartbeat regardless of leader status
                if let Err(e) = election.publish_heartbeat().await {
                    warn!("Heartbeat publish failed: {e}");
                }

                match election.try_acquire_leadership().await {
                    Ok(true) => debug!(instance_id = election.instance_id(), "This instance is leader"),
                    Ok(false) => debug!(instance_id = election.instance_id(), "This instance is follower"),
                    Err(e) => warn!("Leader election error: {e}"),
                }
            }
            _ = process_tick.tick() => {
                // All instances process transactions (SKIP LOCKED handles concurrency)
                if let Err(e) = process_batch(&pool, &horizon_client, 10).await {
                    error!("Processor batch error: {e}");
                }
            }
        }
    }
}
