use crate::services::scheduler::Job;
use crate::stellar::HorizonClient;
use async_trait::async_trait;
use sqlx::PgPool;
use std::error::Error;
use std::io;
use tracing::info;

/// Wrapper for the TransactionProcessor to make it compatible with the Job trait
pub struct TransactionProcessorJob {
    pool: PgPool,
    horizon_client: HorizonClient,
}

impl TransactionProcessorJob {
    pub fn new(pool: PgPool, horizon_client: HorizonClient) -> Self {
        Self {
            pool,
            horizon_client,
        }
    }
}

#[async_trait]
impl Job for TransactionProcessorJob {
    fn name(&self) -> &str {
        "transaction_processor"
    }

    fn schedule(&self) -> &str {
        "*/5 * * * * *" // Every 5 seconds
    }

    async fn execute(&self) -> Result<(), Box<dyn Error + Send + Sync>> {
        info!("Running scheduled transaction processor job");

        // Process a single batch of transactions instead of running continuously.
        // No webhook dispatcher wired here — this job is not currently
        // registered with the scheduler in main.rs (ProcessorPool::start is
        // the live path that dispatches webhooks).
        let feature_flags =
            crate::services::feature_flags::FeatureFlagService::new(self.pool.clone());
        let result = crate::services::processor::process_batch(
            &self.pool,
            &self.horizon_client,
            10,
            None,
            &feature_flags,
        )
        .await;

        match result {
            Ok(_) => {
                info!("Transaction processor job completed successfully");
                Ok(())
            }
            Err(e) => {
                tracing::error!("Transaction processor job failed: {}", e);
                // Convert anyhow::Error to a standard error type
                Err(Box::new(io::Error::other(e.to_string())))
            }
        }
    }
}
