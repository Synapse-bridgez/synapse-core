use sqlx::PgPool;
use tokio::time::{sleep, Duration};
use tracing::{debug, error, info};

use crate::db::models::Transaction;
use crate::stellar::HorizonClient;

const POLL_INTERVAL_SECS: u64 = 5;

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

pub async fn process_batch(pool: &PgPool, _horizon_client: &HorizonClient) -> anyhow::Result<()> {
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
        tx.commit().await?;
        return Ok(());
    }

    debug!("Processing {} pending transaction(s)", pending.len());

    // Process transactions here
    for _transaction in pending {
        // TODO: Implement transaction processing logic
    }

    tx.commit().await?;
    Ok(())
}
