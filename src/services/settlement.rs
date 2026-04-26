use crate::db::models::Settlement;
use crate::db::queries;
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::AppError;
use bigdecimal::BigDecimal;

/// Valid settlement status transitions.
fn valid_transition(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("completed", "pending_review")
            | ("completed", "disputed")
            | ("pending_review", "disputed")
            | ("pending_review", "adjusted")
            | ("pending_review", "voided")
            | ("disputed", "adjusted")
            | ("disputed", "voided")
            | ("disputed", "pending_review")
    )
}

pub struct SettlementService {
    pool: PgPool,
}

impl SettlementService {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Run settlement for all assets with completed, unsettled transactions.
    pub async fn run_settlements(&self) -> Result<Vec<Settlement>, AppError> {
        let assets = queries::get_unique_assets_to_settle(&self.pool)
            .await
            .map_err(|e| AppError::DatabaseError(e.to_string()))?;

        let mut results = Vec::new();
        for asset in assets {
            match self.settle_asset(&asset).await {
                Ok(Some(settlement)) => results.push(settlement),
                Ok(None) => tracing::info!("No transactions to settle for asset {}", asset),
                Err(e) => tracing::error!("Failed to settle asset {}: {:?}", asset, e),
            }
        }

        Ok(results)
    }

    /// Settle transactions for a specific asset.
    pub async fn settle_asset(&self, asset_code: &str) -> Result<Option<Settlement>, AppError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| AppError::DatabaseError(e.to_string()))?;

        // We settle everything up to "now"
        let end_time = Utc::now();

        // Fetch candidate transactions with FOR UPDATE lock
        let unsettled = queries::get_unsettled_transactions(&mut tx, asset_code, end_time)
            .await
            .map_err(|e| AppError::DatabaseError(e.to_string()))?;

        if unsettled.is_empty() {
            tx.rollback()
                .await
                .map_err(|e| AppError::DatabaseError(e.to_string()))?;
            return Ok(None);
        }

        let tx_count = unsettled.len() as i32;
        let total_amount: BigDecimal = unsettled
            .iter()
            .map(|t| t.amount.clone())
            .fold(BigDecimal::from(0), |acc, x| acc + x);

        // Find the range of transactions
        let period_start = unsettled
            .iter()
            .map(|t| t.created_at)
            .min()
            .unwrap_or(end_time);
        let period_end = unsettled
            .iter()
            .map(|t| t.updated_at)
            .max()
            .unwrap_or(end_time);

        let settlement = Settlement {
            id: Uuid::new_v4(),
            asset_code: asset_code.to_string(),
            total_amount,
            tx_count,
            period_start,
            period_end,
            status: "completed".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            dispute_reason: None,
            original_total_amount: None,
            reviewed_by: None,
            reviewed_at: None,
        };

        // Save settlement record
        let saved_settlement = queries::insert_settlement(&mut tx, &settlement)
            .await
            .map_err(|e| AppError::DatabaseError(e.to_string()))?;

        // Link transactions to settlement
        let tx_ids: Vec<Uuid> = unsettled.iter().map(|t| t.id).collect();
        queries::update_transactions_settlement(&mut tx, &tx_ids, saved_settlement.id)
            .await
            .map_err(|e| AppError::DatabaseError(e.to_string()))?;

        tx.commit()
            .await
            .map_err(|e| AppError::DatabaseError(e.to_string()))?;

        // Invalidate cache after successful commit
        queries::invalidate_caches_for_asset(asset_code).await;

        tracing::info!(
            "Settled {} transactions for asset {} (ID: {})",
            tx_count,
            asset_code,
            saved_settlement.id
        );

        Ok(Some(saved_settlement))
    }

    /// Change a settlement's status (dispute, adjust, void, etc.).
    /// Validates the transition, then delegates to the query layer which
    /// handles audit logging and releasing transactions on void.
    pub async fn update_status(
        &self,
        id: Uuid,
        new_status: &str,
        reason: Option<&str>,
        new_total: Option<&BigDecimal>,
        actor: &str,
    ) -> Result<Settlement, AppError> {
        let current = queries::get_settlement(&self.pool, id)
            .await
            .map_err(|e| {
                if matches!(e, sqlx::Error::RowNotFound) {
                    AppError::NotFound(format!("settlement {id}"))
                } else {
                    AppError::DatabaseError(e.to_string())
                }
            })?;

        if !valid_transition(&current.status, new_status) {
            return Err(AppError::BadRequest(format!(
                "invalid transition: {} -> {}",
                current.status, new_status
            )));
        }

        queries::update_settlement_status(&self.pool, id, new_status, reason, new_total, actor)
            .await
            .map_err(|e| AppError::DatabaseError(e.to_string()))
    }
}
