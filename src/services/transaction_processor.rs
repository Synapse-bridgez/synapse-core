use sqlx::PgPool;
use tracing::instrument;

#[derive(Clone)]
pub struct TransactionProcessor {
    pool: PgPool,
}

impl TransactionProcessor {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    #[instrument(name = "processor.process_transaction", skip(self), fields(transaction.id = %tx_id))]
    pub async fn process_transaction(&self, tx_id: uuid::Uuid) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE transactions SET status = 'completed', updated_at = NOW() WHERE id = $1",
        )
        .bind(tx_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    #[instrument(name = "processor.requeue_dlq", skip(self), fields(dlq.id = %dlq_id))]
    pub async fn requeue_dlq(&self, dlq_id: uuid::Uuid) -> anyhow::Result<()> {
        let tx_id: uuid::Uuid =
            sqlx::query_scalar("SELECT transaction_id FROM transaction_dlq WHERE id = $1")
                .bind(dlq_id)
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

        Ok(())
    }
}
