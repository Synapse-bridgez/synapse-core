use crate::services::{LockManager, TransactionProcessor};
use std::time::Duration;
use tracing::{info, warn};
use uuid::Uuid;

/// Example: Process transaction with distributed lock
pub async fn process_transaction_with_lock(
    lock_manager: &LockManager,
    processor: &TransactionProcessor,
    tx_id: Uuid,
) -> anyhow::Result<bool> {
    let resource = format!("transaction:{}", tx_id);
    let timeout = Duration::from_secs(5);

    // Try to acquire lock
    let lock = match lock_manager.acquire(&resource, timeout).await? {
        Some(lock) => lock,
        None => {
            warn!("Could not acquire lock for transaction {}", tx_id);
            return Ok(false);
        }
    };

    info!("Processing transaction {} with lock", tx_id);

    // Process transaction
    let result = processor.process_transaction(tx_id).await;

    // Release lock
    lock.release().await?;

    result.map(|_| true)
}

/// Example: Long-running operation with auto-renewal
pub async fn long_running_with_lock(
    lock_manager: &LockManager,
    resource: &str,
) -> anyhow::Result<()> {
    let timeout = Duration::from_secs(5);

    let lock = match lock_manager.acquire(resource, timeout).await? {
        Some(lock) => lock,
        None => {
            return Err(anyhow::anyhow!("Could not acquire lock"));
        }
    };

    // Spawn auto-renewal task
    let renewal_lock = lock.clone();
    tokio::spawn(async move {
        renewal_lock.auto_renew_task().await;
    });

    // Do long-running work
    tokio::time::sleep(Duration::from_secs(60)).await;

    // Lock will be released on drop
    Ok(())
}

/// Example: Using with_lock helper
pub async fn process_with_helper(
    lock_manager: &LockManager,
    processor: &TransactionProcessor,
    tx_id: Uuid,
) -> anyhow::Result<Option<()>> {
    let resource = format!("transaction:{}", tx_id);
    let timeout = Duration::from_secs(5);

    lock_manager
        .with_lock(&resource, timeout, || {
            Box::pin(async move {
                processor
                    .process_transaction(tx_id)
                    .await
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
            })
        })
        .await
        .map_err(|e| anyhow::anyhow!("Lock error: {}", e))
}
