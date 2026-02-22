use tokio::sync::broadcast;
use uuid::Uuid;

use crate::handlers::ws::TransactionStatusUpdate;

/// Publish a transaction status update to all WebSocket clients
pub fn publish_status_update(
    tx: &broadcast::Sender<TransactionStatusUpdate>,
    transaction_id: Uuid,
    status: String,
    message: Option<String>,
) {
    let update = TransactionStatusUpdate {
        transaction_id,
        status,
        timestamp: chrono::Utc::now(),
        message,
    };

    // Send returns the number of receivers, we don't care if there are none
    match tx.send(update.clone()) {
        Ok(n) => tracing::debug!("Broadcast status update to {} clients", n),
        Err(_) => tracing::trace!("No active WebSocket clients"),
    }
}
