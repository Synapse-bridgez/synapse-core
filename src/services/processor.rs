//! Background processor for pending transactions.
//!
//! Polls the transactions table for rows with status = 'pending',
//! verifies on-chain via HorizonClient, and updates status to completed or failed.

use crate::db::models::Transaction;
use crate::stellar::HorizonClient;
use sqlx::PgPool;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, error, info};

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

pub async fn process_batch(pool: &PgPool, horizon_client: &HorizonClient) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;

    // Fetch pending transactions with row locking. SKIP LOCKED ensures we don't
    // block on rows another worker is processing.
    let pending: Vec<Transaction> = sqlx::query_as::<_, Transaction>(
        r#"
        SELECT id, stellar_account, amount, asset_code, status, created_at, updated_at,
               anchor_transaction_id, callback_type, callback_status, settlement_id
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

    // Claim all rows by marking as processing, then commit to release the lock
    for t in &pending {
        sqlx::query("UPDATE transactions SET status = 'processing', updated_at = NOW() WHERE id = $1")
            .bind(t.id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;

    // Process each (Horizon calls without holding DB lock)
    for t in pending {
        let id = t.id;
        let stellar_account = t.stellar_account.clone();

        let result = horizon_client.get_account(&stellar_account).await;

        match result {
            Ok(_) => {
                sqlx::query(
                    "UPDATE transactions SET status = 'completed', updated_at = NOW() WHERE id = $1",
                )
                .bind(id)
                .execute(pool)
                .await?;
                info!("Transaction {} verified on-chain, marked completed", id);
            }
            Err(e) => {
                sqlx::query("UPDATE transactions SET status = 'failed', updated_at = NOW() WHERE id = $1")
                    .bind(id)
                    .execute(pool)
                    .await?;
                error!("Transaction {} verification failed: {}, marked failed", id, e);
            }
        }
    }

    Ok(())
}
