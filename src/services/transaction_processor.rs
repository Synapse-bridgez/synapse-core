use crate::services::webhook_dispatcher::WebhookDispatcher;
use sqlx::PgPool;
use tracing::instrument;

#[derive(Clone)]
pub struct TransactionProcessor {
    pool: PgPool,
    webhook_dispatcher: Option<WebhookDispatcher>,
}

impl TransactionProcessor {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            webhook_dispatcher: None,
        }
    }

    /// Attach a WebhookDispatcher so state transitions trigger outgoing webhooks.
    pub fn with_webhook_dispatcher(mut self, dispatcher: WebhookDispatcher) -> Self {
        self.webhook_dispatcher = Some(dispatcher);
        self
    }

    #[instrument(name = "processor.process_transaction", skip(self), fields(transaction.id = %tx_id))]
    pub async fn process_transaction(&self, tx_id: uuid::Uuid) -> anyhow::Result<()> {
        // Get asset_code before update for cache invalidation
        let asset_code: String =
            sqlx::query_scalar("SELECT asset_code FROM transactions WHERE id = $1")
                .bind(tx_id)
                .fetch_one(&self.pool)
                .await?;

        sqlx::query(
            "UPDATE transactions SET status = 'completed', updated_at = NOW() WHERE id = $1",
        )
        .bind(tx_id)
        .execute(&self.pool)
        .await?;

        // Invalidate cache after update
        crate::db::queries::invalidate_caches_for_asset(&asset_code).await;

        Ok(())
    }

    #[instrument(name = "processor.requeue_dlq", skip(self), fields(dlq.id = %dlq_id))]
    pub async fn requeue_dlq(&self, dlq_id: uuid::Uuid) -> anyhow::Result<()> {
        let tx_id: uuid::Uuid =
            sqlx::query_scalar("SELECT transaction_id FROM transaction_dlq WHERE id = $1")
                .bind(dlq_id)
                .fetch_one(&self.pool)
                .await?;

        // Get asset_code for cache invalidation
        let asset_code: String =
            sqlx::query_scalar("SELECT asset_code FROM transactions WHERE id = $1")
                .bind(tx_id)
                .fetch_one(&self.pool)
                .await?;

        sqlx::query("UPDATE transactions SET status = 'pending', updated_at = NOW() WHERE id = $1")
            .bind(tx_id)
            .execute(&self.pool)
            .await?;

        sqlx::query("DELETE FROM transaction_dlq WHERE id = $1")
            .bind(dlq_id)
            .execute(&self.pool)
            .await?;

        // Invalidate cache after update
        crate::db::queries::invalidate_caches_for_asset(&asset_code).await;

        Ok(())
    }
}
