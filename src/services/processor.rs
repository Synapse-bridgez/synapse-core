use tokio::sync::broadcast;
use uuid::Uuid;

use crate::handlers::ws::TransactionStatusUpdate;

/// Runs the background processor loop. Processes pending transactions asynchronously
/// without blocking the HTTP server. Uses `SELECT ... FOR UPDATE SKIP LOCKED`
/// for safe concurrent processing with multiple workers.
pub async fn run_processor(pool: PgPool, horizon_client: HorizonClient) {
    info!("Async transaction processor started");

    loop {
        if let Err(e) = process_batch(&pool, &horizon_client).await {
            error!("Processor batch error: {}", e);
        }

        sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
    }
}

async fn process_batch(pool: &PgPool, horizon_client: &HorizonClient) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;

    // Fetch pending transactions with row locking. SKIP LOCKED ensures we don't
    // block on rows another worker is processing.
    let pending: Vec<Transaction> = sqlx::query_as::<_, Transaction>(
        r#"
        SELECT id, stellar_account, amount, asset_code, status, created_at, updated_at,
               anchor_transaction_id, callback_type, callback_status, settlement_id,
               memo, memo_type, metadata
        FROM transactions
        WHERE status = 'pending'
        ORDER BY created_at ASC
        LIMIT 10
        FOR UPDATE SKIP LOCKED
        "#,
    )
    .fetch_all(&mut *tx)
    .await?;

    if pending.is_empty() {
        return Ok(());
    }

    debug!("Processing {} pending transaction(s)", pending.len());

    // Send returns the number of receivers, we don't care if there are none
    match tx.send(update.clone()) {
        Ok(n) => tracing::debug!("Broadcast status update to {} clients", n),
        Err(_) => tracing::trace!("No active WebSocket clients"),
    }
}
