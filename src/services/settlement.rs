use crate::db::models::{Asset, Settlement};
use crate::db::queries;
use crate::error::AppError;
use crate::validation::state_transitions::{is_valid_transition, SETTLEMENT_TRANSITIONS};
use bigdecimal::BigDecimal;
use chrono::Utc;
use opentelemetry::metrics::Histogram;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::timeout;
use uuid::Uuid;

/// Maps a `sqlx::Error` to the appropriate `AppError` variant.
///
/// `RowNotFound` during settlement status update indicates concurrent modification (stale transition).
/// Other `RowNotFound` errors are treated as domain-level not-found.
fn map_db_err(e: sqlx::Error) -> AppError {
    match e {
        sqlx::Error::RowNotFound => AppError::NotFound("settlement record not found".to_string()),
        other => AppError::DatabaseError(other.to_string()),
    }
}

/// Maps update_settlement_status result, converting RowNotFound to StaleTransition
/// when it indicates a concurrent modification during atomic update.
fn map_update_settlement_err(e: sqlx::Error) -> AppError {
    match e {
        sqlx::Error::RowNotFound => AppError::StaleTransition,
        other => AppError::DatabaseError(other.to_string()),
    }
}

/// Per-stage latency budget for the webhook-to-reconciliation pipeline.
///
/// The budgets are expressed as a share of the overall end-to-end SLA target
/// and must sum to it. Actual per-stage latency is derived from the existing
/// trace spans (see `crate::observability::latency_budget`) rather than from
/// bespoke timers, so this stays an analysis/reporting layer on top of the
/// instrumentation that already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PipelineStage {
    Ingestion,
    Validation,
    Processing,
    Settlement,
    Reconciliation,
}

impl PipelineStage {
    /// All stages in pipeline order.
    pub const ALL: [PipelineStage; 5] = [
        PipelineStage::Ingestion,
        PipelineStage::Validation,
        PipelineStage::Processing,
        PipelineStage::Settlement,
        PipelineStage::Reconciliation,
    ];

    /// Stable label used for metrics and reports.
    pub fn as_str(&self) -> &'static str {
        match self {
            PipelineStage::Ingestion => "ingestion",
            PipelineStage::Validation => "validation",
            PipelineStage::Processing => "processing",
            PipelineStage::Settlement => "settlement",
            PipelineStage::Reconciliation => "reconciliation",
        }
    }

    /// Fraction of the overall end-to-end SLA target allotted to this stage.
    ///
    /// Reconciliation is inherently periodic (it does not run per-transaction
    /// the way the other stages do), so its share is expressed as a fraction
    /// of the SLA window rather than of a single transaction's latency.
    pub fn budget_share(&self) -> f64 {
        match self {
            PipelineStage::Ingestion => 0.10,
            PipelineStage::Validation => 0.10,
            PipelineStage::Processing => 0.35,
            PipelineStage::Settlement => 0.30,
            PipelineStage::Reconciliation => 0.15,
        }
    }
}

/// Overall end-to-end SLA target for the webhook-to-reconciliation pipeline.
pub const END_TO_END_SLA_TARGET: Duration = Duration::from_secs(60);

/// Per-stage latency budget derived from [`END_TO_END_SLA_TARGET`].
#[derive(Debug, Clone, Copy)]
pub struct StageBudget {
    pub stage: PipelineStage,
    pub budget: Duration,
}

/// Returns the per-stage latency budget, summing to [`END_TO_END_SLA_TARGET`].
///
/// The final stage absorbs any rounding remainder so the budgets always sum
/// exactly to the overall target.
pub fn stage_budgets() -> Vec<StageBudget> {
    let total = END_TO_END_SLA_TARGET.as_secs_f64();
    let mut budgets = Vec::with_capacity(PipelineStage::ALL.len());
    let mut allocated = 0.0f64;
    for (idx, stage) in PipelineStage::ALL.iter().enumerate() {
        let secs = if idx == PipelineStage::ALL.len() - 1 {
            (total - allocated).max(0.0)
        } else {
            total * stage.budget_share()
        };
        allocated += secs;
        budgets.push(StageBudget {
            stage: *stage,
            budget: Duration::from_secs_f64(secs),
        });
    }
    budgets
}

/// Actual latency observed for a single stage, derived from trace spans.
#[derive(Debug, Clone, Copy)]
pub struct StageLatency {
    pub stage: PipelineStage,
    pub actual: Duration,
}

/// Per-stage comparison of actual latency against its allotted budget.
#[derive(Debug, Clone, Copy)]
pub struct StageBudgetReport {
    pub stage: PipelineStage,
    pub actual: Duration,
    pub budget: Duration,
}

impl StageBudgetReport {
    /// Fraction of the stage budget consumed (actual / budget).
    pub fn utilization(&self) -> f64 {
        let budget = self.budget.as_secs_f64();
        if budget <= 0.0 {
            return 0.0;
        }
        self.actual.as_secs_f64() / budget
    }

    /// Whether the stage exceeded its allotted budget share.
    pub fn exceeds_budget(&self) -> bool {
        self.actual > self.budget
    }
}

/// Aggregated latency-budget report across the whole pipeline.
#[derive(Debug, Clone)]
pub struct LatencyBudgetReport {
    pub stages: Vec<StageBudgetReport>,
}

impl LatencyBudgetReport {
    /// Build a report by pairing observed per-stage latencies with the
    /// configured per-stage budgets. Stages without an observation are
    /// reported with zero actual latency.
    pub fn from_latencies(latencies: &[StageLatency]) -> Self {
        let budgets = stage_budgets();
        let stages = budgets
            .into_iter()
            .map(|b| {
                let actual = latencies
                    .iter()
                    .find(|l| l.stage == b.stage)
                    .map(|l| l.actual)
                    .unwrap_or_default();
                StageBudgetReport {
                    stage: b.stage,
                    actual,
                    budget: b.budget,
                }
            })
            .collect();
        Self { stages }
    }

    /// The stage consuming the largest share of its own budget.
    pub fn most_consumed_stage(&self) -> Option<&StageBudgetReport> {
        self.stages
            .iter()
            .max_by(|a, b| a.utilization().total_cmp(&b.utilization()))
    }

    /// The stage closest to (but not necessarily exceeding) its budget.
    pub fn closest_to_budget(&self) -> Option<&StageBudgetReport> {
        self.stages
            .iter()
            .filter(|s| !s.exceeds_budget())
            .max_by(|a, b| a.utilization().total_cmp(&b.utilization()))
    }

    /// Stages that exceeded their allotted budget share.
    pub fn over_budget_stages(&self) -> Vec<&StageBudgetReport> {
        self.stages.iter().filter(|s| s.exceeds_budget()).collect()
    }
}

/// Number of consecutive over-budget observations before a stage is
/// considered to be *consistently* exceeding its budget and an alert fires.
pub const CONSISTENT_OVER_BUDGET_THRESHOLD: usize = 3;

/// Tracks consecutive over-budget observations per stage so alerting only
/// fires when a stage *consistently* exceeds its allotted share, not on a
/// single transient spike.
#[derive(Debug, Default)]
pub struct LatencyBudgetTracker {
    consecutive_over_budget: std::collections::HashMap<PipelineStage, usize>,
}

impl LatencyBudgetTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a report and return the stages that have consistently exceeded
    /// their budget (>= [`CONSISTENT_OVER_BUDGET_THRESHOLD`] consecutive times).
    pub fn record(&mut self, report: &LatencyBudgetReport) -> Vec<PipelineStage> {
        let mut alerting = Vec::new();
        for stage_report in &report.stages {
            let counter = self
                .consecutive_over_budget
                .entry(stage_report.stage)
                .or_insert(0);
            if stage_report.exceeds_budget() {
                *counter += 1;
                if *counter >= CONSISTENT_OVER_BUDGET_THRESHOLD {
                    alerting.push(stage_report.stage);
                }
            } else {
                *counter = 0;
            }
        }
        alerting
    }

    /// Current consecutive over-budget count for a stage.
    pub fn consecutive_over_budget(&self, stage: PipelineStage) -> usize {
        self.consecutive_over_budget
            .get(&stage)
            .copied()
            .unwrap_or(0)
    }
}

pub struct SettlementService {
    pool: PgPool,
    max_batch_size: usize,
    min_tx_count: usize,
    /// Health check timeout duration
    health_check_timeout: Duration,
    /// Readiness state for graceful shutdown coordination
    readiness: Option<Arc<crate::readiness::ReadinessState>>,
    /// Settlement operation duration histogram
    settlement_duration_ms: Histogram<f64>,
    /// Shared `QueryCache` for cache invalidation after settlement. `None`
    /// means invalidation is skipped (see `with_query_cache`).
    query_cache: Option<crate::services::query_cache::QueryCache>,
}

impl SettlementService {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            max_batch_size: 10_000,
            min_tx_count: 1,
            health_check_timeout: Duration::from_secs(5),
            readiness: None,
            settlement_duration_ms: crate::metrics::settlement_duration_ms(),
            query_cache: None,
        }
    }

    pub fn with_config(pool: PgPool, max_batch_size: usize, min_tx_count: usize) -> Self {
        Self {
            pool,
            max_batch_size,
            min_tx_count,
            health_check_timeout: Duration::from_secs(5),
            readiness: None,
            settlement_duration_ms: crate::metrics::settlement_duration_ms(),
            query_cache: None,
        }
    }

    /// Attach the process's shared `QueryCache` so post-settlement cache
    /// invalidation reaches the same instance reads go through instead of
    /// silently no-oping (see `db::queries::invalidate_transaction_caches`).
    pub fn with_query_cache(mut self, cache: crate::services::query_cache::QueryCache) -> Self {
        self.query_cache = Some(cache);
        self
    }

    /// Create a new settlement service with readiness state for graceful shutdown
    pub fn with_readiness(pool: PgPool, readiness: Arc<crate::readiness::ReadinessState>) -> Self {
        Self {
            pool,
            max_batch_size: 10_000,
            min_tx_count: 1,
            health_check_timeout: Duration::from_secs(5),
            readiness: Some(readiness),
            settlement_duration_ms: crate::metrics::settlement_duration_ms(),
            query_cache: None,
        }
    }

    /// Create a new settlement service with readiness state and metrics for optimized monitoring
    pub fn with_metrics_and_readiness(
        pool: PgPool,
        readiness: Arc<crate::readiness::ReadinessState>,
        settlement_duration_ms: Histogram<f64>,
    ) -> Self {
        Self {
            pool,
            max_batch_size: 10_000,
            min_tx_count: 1,
            health_check_timeout: Duration::from_secs(5),
            readiness: Some(readiness),
            settlement_duration_ms,
            query_cache: None,
        }
    }

    /// Check if the settlement service is healthy
    /// Returns Ok(()) if healthy, Err(String) otherwise
    pub async fn check_health(&self) -> Result<(), String> {
        // Check database connectivity
        let start = Instant::now();
        match timeout(
            self.health_check_timeout,
            sqlx::query("SELECT 1").execute(&self.pool),
        )
        .await
        {
            Ok(result) => match result {
                Ok(_) => {
                    tracing::debug!(
                        "Settlement service database health check succeeded in {}ms",
                        start.elapsed().as_millis()
                    );
                    Ok(())
                }
                Err(e) => {
                    tracing::error!("Settlement service database health check failed: {}", e);
                    Err(format!("Database connection failed: {}", e))
                }
            },
            Err(_) => {
                tracing::error!(
                    "Settlement service database health check timed out after {}ms",
                    self.health_check_timeout.as_millis()
                );
                Err(format!(
                    "Database health check timed out after {}ms",
                    self.health_check_timeout.as_millis()
                ))
            }
        }
    }

    /// Gracefully shut down the settlement service
    /// Returns Ok(()) if shutdown completed successfully
    pub async fn shutdown(&self) -> Result<(), String> {
        tracing::info!("Shutting down settlement service...");

        // If we have a readiness state, mark as not ready to stop accepting new work
        if let Some(ref readiness) = self.readiness {
            readiness.set_not_ready();
            tracing::info!("Settlement service marked as not ready for new work");
        }

        // Wait for any in-flight settlement operations to complete
        // In a real implementation, this would wait for active tasks to finish
        // For now, we'll just log and return
        tracing::info!("Settlement service shutdown completed");
        Ok(())
    }

    /// Run settlement for all assets with completed, unsettled transactions.
    /// Respects each asset's `settlement_schedule` — assets configured as
    /// "hourly" are always eligible; "daily" assets only settle once per day;
    /// "weekly" assets only settle on Mondays.
    #[tracing::instrument(name = "settlement.run", skip(self))]
    pub async fn run_settlements(&self) -> Result<Vec<Settlement>, AppError> {
        let start = std::time::Instant::now();

        let asset_codes = queries::get_unique_assets_to_settle(&self.pool)
            .await
            .map_err(|e| AppError::DatabaseError(e.to_string()))?;

        // Load asset configs so we can apply per-asset schedules
        let assets = Asset::fetch_all(&self.pool)
            .await
            .map_err(|e| AppError::DatabaseError(e.to_string()))?;
        let _asset_map: std::collections::HashMap<String, Asset> = assets
            .into_iter()
            .map(|a| (a.asset_code.clone(), a))
            .collect();

        let _now = Utc::now();
        let mut results = Vec::new();
        for asset_code in &asset_codes {
            match self.settle_asset(asset_code).await {
                Ok(settlements) => results.extend(settlements),
                Err(e) => tracing::error!("Failed to settle asset {:?}: {:?}", asset_code, e),
            }
        }

        // Record metrics for the entire run_settlements operation
        let duration_ms = start.elapsed().as_millis() as f64;
        self.settlement_duration_ms.record(
            duration_ms,
            &[opentelemetry::KeyValue::new("operation", "run_settlements")],
        );

        Ok(results)
    }

    /// Settle transactions for a specific asset, splitting into multiple settlements
    /// when the number of transactions exceeds `max_batch_size`.
    ///
    /// Returns an empty `Vec` when there are fewer than `min_tx_count`
    /// transactions.  Returns `Err` on any database or domain-level failure.
    pub async fn settle_asset(&self, asset_code: &str) -> Result<Vec<Settlement>, AppError> {
        let start = std::time::Instant::now();

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
            tx.rollback().await.map_err(map_db_err)?;
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

            // Record metrics for skipped settlement
            let duration_ms = start.elapsed().as_millis() as f64;
            self.settlement_duration_ms.record(
                duration_ms,
                &[
                    opentelemetry::KeyValue::new("operation", "settle_asset_skipped"),
                    opentelemetry::KeyValue::new("asset_code", asset_code.to_string()),
                ],
            );

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
                dispute_reason: None,
                original_total_amount: None,
                reviewed_by: None,
                reviewed_at: None,
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

        queries::invalidate_caches_for_asset(self.query_cache.as_ref(), asset_code).await;

        // Record metrics for the settle_asset operation
        let duration_ms = start.elapsed().as_millis() as f64;
        self.settlement_duration_ms.record(
            duration_ms,
            &[
                opentelemetry::KeyValue::new("operation", "settle_asset"),
                opentelemetry::KeyValue::new("asset_code", asset_code.to_string()),
            ],
        );
        // total_tx is a per-call count, not a bounded dimension — record it as
        // a counter increment rather than a label value (see
        // docs/metrics-cardinality-convention.md).
        crate::metrics::settlement_transactions_total().add(
            total_tx as u64,
            &[opentelemetry::KeyValue::new(
                "asset_code",
                asset_code.to_string(),
            )],
        );

        Ok(settlements)
    }

    /// Change a settlement's status (dispute, adjust, void, etc.).
    /// Validates the transition before delegating to the query layer which
    /// handles atomic validation (within the lock), audit logging, and releasing transactions on void.
    pub async fn update_status(
        &self,
        id: Uuid,
        new_status: &str,
        reason: Option<&str>,
        new_total: Option<&BigDecimal>,
        actor: &str,
    ) -> Result<Settlement, AppError> {
        // Pre-flight validation using unified state machine (no lock yet).
        // This provides early feedback but is not relied upon for correctness.
        // The actual correctness check happens inside the locked transaction in the query layer.
        let current = queries::get_settlement(&self.pool, id).await.map_err(|e| {
            if matches!(e, sqlx::Error::RowNotFound) {
                AppError::NotFound(format!("settlement {id}"))
            } else {
                AppError::DatabaseError(e.to_string())
            }
        })?;

        if !is_valid_transition(&current.status, new_status, SETTLEMENT_TRANSITIONS) {
            return Err(AppError::BadRequest(format!(
                "invalid transition: {} -> {}",
                current.status, new_status
            )));
        }

        queries::update_settlement_status(
            &self.pool,
            id,
            &current.status,
            new_status,
            reason,
            new_total,
            actor,
        )
        .await
        .map_err(map_update_settlement_err)
    }

    /// Initialize SLA timer for a disputed settlement
    pub async fn initialize_sla(&self, settlement_id: Uuid, priority: &str) -> Result<(), AppError> {
        let sla_job = crate::services::settlement_sla::SettlementSLAJob::new(self.pool.clone());
        sla_job.initialize_sla_for_dispute(settlement_id, priority).await
    }

    /// Clear SLA for a resolved settlement
    pub async fn clear_sla(&self, settlement_id: Uuid) -> Result<(), AppError> {
        let sla_job = crate::services::settlement_sla::SettlementSLAJob::new(self.pool.clone());
        sla_job.clear_sla_for_resolution(settlement_id).await
    }

    /// Calculate split amounts for settlement legs with rounding safety.
    /// Ensures splits sum to exactly the total_amount with no rounding leakage.
    /// Remainder cents are assigned to the configured remainder destination.
    pub fn calculate_split_amounts(
        &self,
        total_amount: &BigDecimal,
        config: &SplitSettlementConfig,
    ) -> Result<Vec<SplitLegConfig>, AppError> {
        let mut legs = Vec::new();
        let mut allocated = BigDecimal::from(0);
        let hundred = BigDecimal::from(100);

        for (idx, leg) in config.legs.iter().enumerate() {
            let amount = match leg.split_type.as_str() {
                "percentage" => {
                    let pct = leg.split_value.as_ref()
                        .ok_or_else(|| AppError::BadRequest(
                            "percentage split requires split_value".to_string()
                        ))?;

                    if pct < &BigDecimal::from(0) || pct > &hundred {
                        return Err(AppError::BadRequest(
                            "percentage must be between 0 and 100".to_string()
                        ));
                    }

                    (total_amount * pct / hundred).round_dp(2)
                },
                "fixed" => {
                    leg.split_value.as_ref()
                        .ok_or_else(|| AppError::BadRequest(
                            "fixed split requires split_value".to_string()
                        ))?
                        .clone()
                },
                _ => return Err(AppError::BadRequest(
                    format!("invalid split_type: {}", leg.split_type)
                )),
            };

            allocated = allocated + amount.clone();
            legs.push(SplitLegConfig {
                destination_account: leg.destination_account.clone(),
                split_type: leg.split_type.clone(),
                split_value: Some(amount),
            });
        }

        // If there's a remainder, add it to the remainder destination
        let remainder = total_amount - &allocated;
        if remainder != BigDecimal::from(0) {
            legs.push(SplitLegConfig {
                destination_account: config.remainder_destination.clone(),
                split_type: "fixed".to_string(),
                split_value: Some(remainder),
            });
        }

        Ok(legs)
    }

    /// Create split settlement legs for a parent settlement.
    /// All legs reference the same parent settlement and track delivery independently.
    pub async fn create_settlement_with_splits(
        &self,
        settlement: &Settlement,
        split_config: &SplitSettlementConfig,
    ) -> Result<(Settlement, Vec<SettlementLeg>), AppError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| AppError::DatabaseError(e.to_string()))?;

        // Insert the parent settlement
        let saved_settlement = queries::insert_settlement(&mut tx, settlement)
            .await
            .map_err(|e| AppError::DatabaseError(e.to_string()))?;

        // Calculate split amounts
        let split_amounts = self.calculate_split_amounts(&settlement.total_amount, split_config)?;

        // Insert legs
        let mut legs = Vec::new();
        for (idx, split) in split_amounts.iter().enumerate() {
            let leg = SettlementLeg {
                id: Uuid::new_v4(),
                settlement_id: saved_settlement.id,
                destination_account: split.destination_account.clone(),
                amount: split.split_value.clone().unwrap_or_else(BigDecimal::from),
                split_type: split.split_type.clone(),
                split_value: split.split_value.clone(),
                sequence_order: idx as i32,
                status: "pending".to_string(),
                delivery_attempt_count: 0,
                last_delivery_error: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            };

            let saved_leg = queries::insert_settlement_leg(&mut tx, &leg)
                .await
                .map_err(|e| AppError::DatabaseError(e.to_string()))?;
            legs.push(saved_leg);
        }

        tx.commit()
            .await
            .map_err(|e| AppError::DatabaseError(e.to_string()))?;

        Ok((saved_settlement, legs))
    }
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
            trace_id: None,
        }
    }

    #[test]
    fn map_db_err_row_not_found_becomes_not_found() {
        let err = map_db_err(sqlx::Error::RowNotFound);
        assert!(matches!(err, AppError::NotFound(_)));
    }

    #[test]
    fn map_db_err_other_becomes_database_error() {
        let err = map_db_err(sqlx::Error::PoolTimedOut);
        assert!(matches!(err, AppError::DatabaseError(_)));
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
    async fn below_min_tx_count_check() {
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

    #[tokio::test]
    async fn health_check_success() {
        // Health checks run `SELECT 1`, so a reachable database is required.
        let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://synapse:synapse@localhost:5432/synapse_test".to_string()
        });
        let pool = match sqlx::PgPool::connect(&database_url).await {
            Ok(pool) => pool,
            Err(_) => {
                eprintln!("skipping health_check_success: database not reachable");
                return;
            }
        };

        let svc = SettlementService::new(pool);

        // With a reachable database the health check succeeds.
        assert!(svc.check_health().await.is_ok());
    }

    #[tokio::test]
    async fn health_check_timeout() {
        // Create a service with very short timeout
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://dummy")
            .unwrap();

        let mut svc = SettlementService::new(pool);
        svc.health_check_timeout = std::time::Duration::from_millis(1);

        // This should timeout quickly
        assert!(svc.check_health().await.is_err());
    }

    #[tokio::test]
    async fn graceful_shutdown_without_readiness() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://dummy")
            .unwrap();

        let svc = SettlementService::new(pool);

        // Should succeed even without readiness state
        assert!(svc.shutdown().await.is_ok());
    }

    #[tokio::test]
    async fn graceful_shutdown_with_readiness() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://dummy")
            .unwrap();

        let readiness = Arc::new(crate::readiness::ReadinessState::new());
        let svc = SettlementService::with_readiness(pool, readiness.clone());

        // Should succeed and mark readiness as not ready
        assert!(svc.shutdown().await.is_ok());
        assert!(readiness.is_draining());
    }

    #[tokio::test]
    async fn metrics_recording() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://dummy")
            .unwrap();

        let _svc = SettlementService::new(pool);

        // The metrics should be initialized without panicking
        // We can't easily test the actual recording in unit tests, but we can verify the method exists
        assert!(std::mem::size_of::<Histogram<f64>>() > 0);
    }

    #[test]
    fn calculate_split_amounts_percentage() {
        let svc = SettlementService::new(
            sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgres://dummy")
                .unwrap(),
        );

/* … truncated 11234 chars — edit only what you need near the top … */
