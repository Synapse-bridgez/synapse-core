use crate::services::webhook_dispatcher::WebhookDispatcher;
use sqlx::PgPool;
use tracing::instrument;

#[async_trait::async_trait]
pub trait ProcessingStage: Send + Sync {
    async fn execute(&self, tx: &crate::db::models::Transaction) -> Result<(), anyhow::Error>;
    fn name(&self) -> &'static str;
}

pub struct ValidateStage;

#[async_trait::async_trait]
impl ProcessingStage for ValidateStage {
    async fn execute(&self, tx: &crate::db::models::Transaction) -> Result<(), anyhow::Error> {
        // Basic validation: check if transaction is in pending status
        if tx.status != "pending" {
            anyhow::bail!("Transaction is not in pending status");
        }
        tracing::info!("Validation stage passed for transaction {}", tx.id);
        Ok(())
    }

    fn name(&self) -> &'static str {
        "validate"
    }
}

pub struct EnrichStage;

#[async_trait::async_trait]
impl ProcessingStage for EnrichStage {
    async fn execute(&self, tx: &crate::db::models::Transaction) -> Result<(), anyhow::Error> {
        // Enrichment logic: could add additional metadata, validate external data, etc.
        // For now, just log
        tracing::info!("Enrichment stage completed for transaction {}", tx.id);
        Ok(())
    }

    fn name(&self) -> &'static str {
        "enrich"
    }
}

pub struct VerifyStage;

#[async_trait::async_trait]
impl ProcessingStage for VerifyStage {
    async fn execute(&self, tx: &crate::db::models::Transaction) -> Result<(), anyhow::Error> {
        // Verification logic: could verify with external systems, check balances, etc.
        // For now, just log
        tracing::info!("Verification stage completed for transaction {}", tx.id);
        Ok(())
    }

    fn name(&self) -> &'static str {
        "verify"
    }
}

pub struct CompleteStage {
    pool: PgPool,
}

impl CompleteStage {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl ProcessingStage for CompleteStage {
    async fn execute(&self, tx: &crate::db::models::Transaction) -> Result<(), anyhow::Error> {
        // Get asset_code before update for cache invalidation
        let asset_code: String =
            sqlx::query_scalar("SELECT asset_code FROM transactions WHERE id = $1")
                .bind(tx.id)
                .fetch_one(&self.pool)
                .await?;

        // Validate status transition: current status → completed
        crate::validation::state_machine::validate_status_transition(&tx.status, "completed")
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        sqlx::query(
            "UPDATE transactions SET status = 'completed', updated_at = NOW() WHERE id = $1",
        )
        .bind(tx.id)
        .execute(&self.pool)
        .await?;

        // Invalidate cache after update
        crate::db::queries::invalidate_caches_for_asset(&asset_code).await;

        tracing::info!("Completion stage completed for transaction {}", tx.id);
        Ok(())
    }

    fn name(&self) -> &'static str {
        "complete"
    }
}

#[derive(Clone)]
pub struct TransactionProcessor {
    pool: PgPool,
    webhook_dispatcher: Option<WebhookDispatcher>,
    feature_flags: crate::services::feature_flags::FeatureFlagService,
}

impl TransactionProcessor {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool: pool.clone(),
            webhook_dispatcher: None,
            feature_flags: crate::services::feature_flags::FeatureFlagService::new(pool),
        }
    }

    /// Attach a WebhookDispatcher so state transitions trigger outgoing webhooks.
    pub fn with_webhook_dispatcher(mut self, dispatcher: WebhookDispatcher) -> Self {
        self.webhook_dispatcher = Some(dispatcher);
        self
    }

    #[instrument(name = "processor.process_transaction", skip(self), fields(transaction.id = %tx_id))]
    pub async fn process_transaction(&self, tx_id: uuid::Uuid) -> anyhow::Result<()> {
        // Fetch the transaction first
        let tx: crate::db::models::Transaction =
            sqlx::query_as("SELECT * FROM transactions WHERE id = $1")
                .bind(tx_id)
                .fetch_one(&self.pool)
                .await?;

        // Define the pipeline stages
        let mut stages: Vec<Box<dyn ProcessingStage>> = Vec::new();

        // Validate stage - always enabled
        stages.push(Box::new(ValidateStage));

        // Enrich stage - feature flagged
        if self
            .feature_flags
            .is_enabled("transaction_enrich_stage")
            .await
            .unwrap_or(false)
        {
            stages.push(Box::new(EnrichStage));
        }

        // Verify stage - feature flagged
        if self
            .feature_flags
            .is_enabled("transaction_verify_stage")
            .await
            .unwrap_or(false)
        {
            stages.push(Box::new(VerifyStage));
        }

        // Complete stage - always enabled
        stages.push(Box::new(CompleteStage::new(self.pool.clone())));

        // Execute the pipeline
        for stage in stages {
            let stage_name = stage.name();
            let start = std::time::Instant::now();
            tracing::info!("Starting {} stage for transaction {}", stage_name, tx_id);

            match stage.execute(&tx).await {
                Ok(()) => {
                    let duration = start.elapsed();
                    tracing::info!(
                        "{} stage completed in {:?} for transaction {}",
                        stage_name,
                        duration,
                        tx_id
                    );
                }
                Err(e) => {
                    tracing::error!(
                        "{} stage failed for transaction {}: {}",
                        stage_name,
                        tx_id,
                        e
                    );
                    // Move to DLQ on failure
                    self.move_to_dlq(tx_id, &format!("{stage_name} stage failed: {e}"))
                        .await?;
                    return Err(e);
                }
            }
        }

        Ok(())
    }

    async fn move_to_dlq(&self, tx_id: uuid::Uuid, reason: &str) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO transaction_dlq (transaction_id, reason, created_at) VALUES ($1, $2, NOW())",
        )
        .bind(tx_id)
        .bind(reason)
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

        // Get current status and asset_code
        let (current_status, asset_code): (String, String) =
            sqlx::query_as("SELECT status, asset_code FROM transactions WHERE id = $1")
                .bind(tx_id)
                .fetch_one(&self.pool)
                .await?;

        // Validate status transition: current status → pending
        crate::validation::state_machine::validate_status_transition(&current_status, "pending")
            .map_err(|e| anyhow::anyhow!("{e}"))?;

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
