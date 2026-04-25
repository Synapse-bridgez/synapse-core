use crate::db::models::Settlement;
use crate::db::queries;
use crate::error::AppError;
use bigdecimal::BigDecimal;
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

pub struct SettlementService {
    pool: PgPool,
    max_batch_size: usize,
    min_tx_count: usize,
}

impl SettlementService {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            max_batch_size: 10_000,
            min_tx_count: 1,
        }
    }

    pub fn with_config(pool: PgPool, max_batch_size: usize, min_tx_count: usize) -> Self {
        Self {
            pool,
            max_batch_size,
            min_tx_count,
        }
    }

    /// Run settlement for all assets with completed, unsettled transactions.
    pub async fn run_settlements(&self) -> Result<Vec<Settlement>, AppError> {
        let assets = queries::get_unique_assets_to_settle(&self.pool)
            .await
            .map_err(|e| AppError::DatabaseError(e.to_string()))?;

        let mut results = Vec::new();
        for asset in assets {
            match self.settle_asset(&asset).await {
                Ok(settlements) => results.extend(settlements),
                Err(e) => tracing::error!("Failed to settle asset {}: {:?}", asset, e),
            }
        }

        Ok(results)
    }

    /// Settle transactions for a specific asset, splitting into multiple settlements
    /// when the number of transactions exceeds `max_batch_size`.
    ///
    /// Returns an empty `Vec` when there are fewer than `min_tx_count` transactions.
    pub async fn settle_asset(&self, asset_code: &str) -> Result<Vec<Settlement>, AppError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| AppError::DatabaseError(e.to_string()))?;

        let end_time = Utc::now();

        let unsettled = queries::get_unsettled_transactions(&mut tx, asset_code, end_time)
            .await
            .map_err(|e| AppError::DatabaseError(e.to_string()))?;

        if unsettled.len() < self.min_tx_count {
            tx.rollback()
                .await
                .map_err(|e| AppError::DatabaseError(e.to_string()))?;
            if unsettled.is_empty() {
                tracing::info!("No transactions to settle for asset {}", asset_code);
            } else {
                tracing::info!(
                    "Skipping settlement for asset {}: {} transaction(s) below minimum {}",
                    asset_code,
                    unsettled.len(),
                    self.min_tx_count
                );
            }
            return Ok(vec![]);
        }

        let total_tx = unsettled.len();
        let batch_count = total_tx.div_ceil(self.max_batch_size);
        tracing::info!(
            asset = %asset_code,
            total_transactions = total_tx,
            batch_size = self.max_batch_size,
            batches = batch_count,
            "Starting settlement"
        );

        let mut settlements = Vec::with_capacity(batch_count);

        for (batch_idx, chunk) in unsettled.chunks(self.max_batch_size).enumerate() {
            let tx_count = chunk.len() as i32;
            let total_amount: BigDecimal = chunk
                .iter()
                .map(|t| t.amount.clone())
                .fold(BigDecimal::from(0), |acc, x| acc + x);

            let period_start = chunk.iter().map(|t| t.created_at).min().unwrap_or(end_time);
            let period_end = chunk.iter().map(|t| t.updated_at).max().unwrap_or(end_time);

            let settlement = Settlement {
                id: Uuid::new_v4(),
                asset_code: asset_code.to_string(),
                total_amount: total_amount.clone(),
                tx_count,
                period_start,
                period_end,
                status: "completed".to_string(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            };

            let saved = queries::insert_settlement(&mut tx, &settlement)
                .await
                .map_err(|e| AppError::DatabaseError(e.to_string()))?;

            let tx_ids: Vec<Uuid> = chunk.iter().map(|t| t.id).collect();
            queries::update_transactions_settlement(&mut tx, &tx_ids, saved.id)
                .await
                .map_err(|e| AppError::DatabaseError(e.to_string()))?;

            tracing::info!(
                asset = %asset_code,
                settlement_id = %saved.id,
                batch = batch_idx + 1,
                total_batches = batch_count,
                tx_count,
                total_amount = %total_amount,
                "Settlement batch created"
            );

            settlements.push(saved);
        }

        tx.commit()
            .await
            .map_err(|e| AppError::DatabaseError(e.to_string()))?;

        queries::invalidate_caches_for_asset(asset_code).await;

        Ok(settlements)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bigdecimal::FromPrimitive;
    use chrono::Utc;
    use uuid::Uuid;

    fn make_tx(amount: f64) -> crate::db::models::Transaction {
        let now = Utc::now();
        crate::db::models::Transaction {
            id: Uuid::new_v4(),
            stellar_account: "GABC".to_string(),
            amount: BigDecimal::from_f64(amount).unwrap(),
            asset_code: "USD".to_string(),
            status: "completed".to_string(),
            created_at: now,
            updated_at: now,
            anchor_transaction_id: None,
            callback_type: None,
            callback_status: None,
            settlement_id: None,
            memo: None,
            memo_type: None,
            metadata: None,
        }
    }

    #[test]
    fn batch_split_logic() {
        // 25 transactions with max_batch_size=10 → 3 batches (10, 10, 5)
        let txs: Vec<_> = (0..25).map(|_| make_tx(1.0)).collect();
        let chunks: Vec<_> = txs.chunks(10).collect();
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].len(), 10);
        assert_eq!(chunks[1].len(), 10);
        assert_eq!(chunks[2].len(), 5);
    }

    #[tokio::test]
    async fn below_min_tx_count_skipped() {
        let svc = SettlementService::with_config(
            sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgres://dummy")
                .unwrap(),
            10_000,
            5,
        );
        assert!(3 < svc.min_tx_count);
    }

    #[tokio::test]
    async fn default_config_values() {
        let svc = SettlementService::with_config(
            sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgres://dummy")
                .unwrap(),
            10_000,
            1,
        );
        assert_eq!(svc.max_batch_size, 10_000);
        assert_eq!(svc.min_tx_count, 1);
    }
}
