use crate::services::query_cache::QueryCache;
use crate::services::webhook_dispatcher::WebhookDispatcher;
use sqlx::PgPool;
use tracing::{instrument, Instrument};

/// Overall end-to-end SLA target for the webhook-to-reconciliation pipeline.
pub const END_TO_END_SLA_TARGET: Duration = Duration::from_secs(30);

/// A single stage of the webhook-to-reconciliation pipeline whose latency
/// budget we track. Distinct from the trace-ID propagation work: trace IDs
/// give correlation, this gives stage-level budget accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LatencyStage {
    Ingestion,
    Validation,
    Processing,
    Settlement,
    Reconciliation,
}

impl LatencyStage {
    pub fn as_str(&self) -> &'static str {
        match self {
            LatencyStage::Ingestion => "ingestion",
            LatencyStage::Validation => "validation",
            LatencyStage::Processing => "processing",
            LatencyStage::Settlement => "settlement",
            LatencyStage::Reconciliation => "reconciliation",
        }
    }

    /// All stages in pipeline order.
    pub fn all() -> [LatencyStage; 5] {
        [
            LatencyStage::Ingestion,
            LatencyStage::Validation,
            LatencyStage::Processing,
            LatencyStage::Settlement,
            LatencyStage::Reconciliation,
        ]
    }
}

/// Per-stage latency budget, summing to `END_TO_END_SLA_TARGET`.
///
/// Reconciliation is inherently periodic (it does not run per-transaction the
/// way the other stages do), so its budget is expressed as the per-cycle
/// share of the end-to-end target rather than a per-transaction allowance.
#[derive(Debug, Clone, Copy)]
pub struct LatencyBudget {
    pub ingestion: Duration,
    pub validation: Duration,
    pub processing: Duration,
    pub settlement: Duration,
    pub reconciliation: Duration,
}

impl Default for LatencyBudget {
    fn default() -> Self {
        Self {
            ingestion: Duration::from_secs(2),
            validation: Duration::from_secs(3),
            processing: Duration::from_secs(10),
            settlement: Duration::from_secs(10),
            reconciliation: Duration::from_secs(5),
        }
    }
}

impl LatencyBudget {
    pub fn for_stage(&self, stage: LatencyStage) -> Duration {
        match stage {
            LatencyStage::Ingestion => self.ingestion,
            LatencyStage::Validation => self.validation,
            LatencyStage::Processing => self.processing,
            LatencyStage::Settlement => self.settlement,
            LatencyStage::Reconciliation => self.reconciliation,
        }
    }

    /// Sum of all per-stage budgets; must equal `END_TO_END_SLA_TARGET`.
    pub fn total(&self) -> Duration {
        self.ingestion + self.validation + self.processing + self.settlement + self.reconciliation
    }
}

/// A single observed stage span, derived from existing trace spans rather
/// than a bespoke timer.
#[derive(Debug, Clone, Copy)]
pub struct StageSpan {
    pub stage: LatencyStage,
    pub duration: Duration,
}

/// Per-stage latency report: actual vs. budget, plus which stage consumes the
/// most budget and which is closest to exceeding it.
#[derive(Debug, Clone)]
pub struct LatencyReport {
    pub budget: LatencyBudget,
    pub actual: HashMap<LatencyStage, Duration>,
    pub utilization: HashMap<LatencyStage, f64>,
    pub most_consumed: Option<LatencyStage>,
    pub closest_to_budget: Option<LatencyStage>,
    pub over_budget: Vec<LatencyStage>,
}

/// Analysis/reporting layer over existing trace spans. Derives per-stage
/// latency from spans and reports it against the per-stage budget.
pub struct LatencyBudgetTracker {
    budget: LatencyBudget,
}

impl Default for LatencyBudgetTracker {
    fn default() -> Self {
        Self::new(LatencyBudget::default())
    }
}

impl LatencyBudgetTracker {
    pub fn new(budget: LatencyBudget) -> Self {
        Self { budget }
    }

    pub fn budget(&self) -> &LatencyBudget {
        &self.budget
    }

    /// Build a report from observed stage spans. Spans for the same stage are
    /// summed (a stage may emit more than one span per pipeline run).
    pub fn report(&self, spans: &[StageSpan]) -> LatencyReport {
        let mut actual: HashMap<LatencyStage, Duration> = HashMap::new();
        for span in spans {
            *actual.entry(span.stage).or_insert(Duration::ZERO) += span.duration;
        }

        let mut utilization: HashMap<LatencyStage, f64> = HashMap::new();
        let mut over_budget: Vec<LatencyStage> = Vec::new();
        let mut most_consumed: Option<(LatencyStage, Duration)> = None;
        let mut closest_to_budget: Option<(LatencyStage, f64)> = None;

        for stage in LatencyStage::all() {
            let observed = actual.get(&stage).copied().unwrap_or(Duration::ZERO);
            let allotted = self.budget.for_stage(stage);
            let ratio = if allotted.is_zero() {
                0.0
            } else {
                observed.as_secs_f64() / allotted.as_secs_f64()
            };
            utilization.insert(stage, ratio);

            if ratio > 1.0 {
                over_budget.push(stage);
            }

            match most_consumed {
                Some((_, d)) if d >= observed => {}
                _ => most_consumed = Some((stage, observed)),
            }

            match closest_to_budget {
                Some((_, r)) if r >= ratio => {}
                _ => closest_to_budget = Some((stage, ratio)),
            }
        }

        LatencyReport {
            budget: self.budget,
            actual,
            utilization,
            most_consumed: most_consumed.map(|(s, _)| s),
            closest_to_budget: closest_to_budget.map(|(s, _)| s),
            over_budget,
        }
    }

    /// Emit metrics for a report and alert when a stage consistently exceeds
    /// its allotted share. `consecutive_over_budget` counts how many recent
    /// reports a stage has exceeded its budget; alerting fires once a stage
    /// crosses `alert_threshold` consecutive breaches.
    pub fn record_and_alert(
        &self,
        report: &LatencyReport,
        consecutive_over_budget: &mut HashMap<LatencyStage, u32>,
        alert_threshold: u32,
    ) {
        for stage in LatencyStage::all() {
            let observed = report.actual.get(&stage).copied().unwrap_or(Duration::ZERO);
            let allotted = self.budget.for_stage(stage);
            let ratio = report.utilization.get(&stage).copied().unwrap_or(0.0);

            crate::metrics::transaction_processor_stage_latency_seconds()
                .record(observed.as_secs_f64(), &[opentelemetry::KeyValue::new("stage", stage.as_str())]);
            crate::metrics::transaction_processor_stage_latency_budget_seconds()
                .record(allotted.as_secs_f64(), &[opentelemetry::KeyValue::new("stage", stage.as_str())]);

            let counter = consecutive_over_budget.entry(stage).or_insert(0);
            if ratio > 1.0 {
                *counter += 1;
                if *counter >= alert_threshold {
                    tracing::warn!(
                        stage = stage.as_str(),
                        observed_seconds = observed.as_secs_f64(),
                        budget_seconds = allotted.as_secs_f64(),
                        consecutive = *counter,
                        "latency budget exceeded for stage"
                    );
                    crate::metrics::transaction_processor_stage_latency_budget_exceeded_total()
                        .add(1, &[opentelemetry::KeyValue::new("stage", stage.as_str())]);
                }
            } else {
                *counter = 0;
            }
        }
    }
}

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
    query_cache: Option<QueryCache>,
}

impl CompleteStage {
    pub fn new(pool: PgPool, query_cache: Option<QueryCache>) -> Self {
        Self { pool, query_cache }
    }
}

#[async_trait::async_trait]
impl ProcessingStage for CompleteStage {
    async fn execute(&self, tx: &crate::db::models::Transaction) -> Result<(), anyhow::Error> {
        // Hold the row lock across the read-decide-write sequence, matching
        // the discipline processor.rs's live batch path uses (SELECT ...
        // FOR UPDATE). Without it, two concurrent completions of the same
        // transaction would both pass validation and both issue an
        // unconditional UPDATE, the second silently overwriting the first.
        let mut db_tx = self.pool.begin().await?;

        let asset_code: String =
            sqlx::query_scalar("SELECT asset_code FROM transactions WHERE id = $1 FOR UPDATE")
                .bind(tx.id)
                .fetch_one(&mut *db_tx)
                .await?;

        // Validate status transition: current status → completed
        crate::validation::state_machine::validate_status_transition(&tx.status, "completed")
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        // WHERE status = $2 is defense in depth: the row lock above already
        // serializes concurrent CompleteStage callers, but this also catches
        // any other write path that changed status without going through it.
        let result = sqlx::query(
            "UPDATE transactions SET status = 'completed', updated_at = NOW() WHERE id = $1 AND status = $2",
        )
        .bind(tx.id)
        .bind(&tx.status)
        .execute(&mut *db_tx)
        .await?;

        if result.rows_affected() == 0 {
            db_tx.rollback().await.ok();
            crate::metrics::transaction_processor_completion_conflict_prevented_total().add(1, &[]);
            anyhow::bail!(
                "Transaction {} status changed before completion could be applied \
                 (concurrent writer won the race)",
                tx.id
            );
        }

        db_tx.commit().await?;

        // Invalidate cache after commit
        crate::db::queries::invalidate_caches_for_asset(self.query_cache.as_ref(), &asset_code)
            .await;

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
    query_cache: Option<QueryCache>,
    feature_flags: crate::services::feature_flags::FeatureFlagService,
    latency_tracker: std::sync::Arc<LatencyBudgetTracker>,
}

impl TransactionProcessor {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool: pool.clone(),
            webhook_dispatcher: None,
            query_cache: None,
            feature_flags: crate::services::feature_flags::FeatureFlagService::new(pool),
            latency_tracker: std::sync::Arc::new(LatencyBudgetTracker::default()),
        }
    }

    /// Attach a WebhookDispatcher so state transitions trigger outgoing webhooks.
    pub fn with_webhook_dispatcher(mut self, dispatcher: WebhookDispatcher) -> Self {
        self.webhook_dispatcher = Some(dispatcher);
        self
    }

    /// Attach the process's shared `QueryCache` so completion/requeue cache
    /// invalidation reaches the same instance reads go through instead of
    /// silently no-oping (see `db::queries::invalidate_transaction_caches`).
    pub fn with_query_cache(mut self, cache: QueryCache) -> Self {
        self.query_cache = Some(cache);
        self
    }

    /// Override the per-stage latency budget used for reporting/alerting.
    pub fn with_latency_budget(mut self, budget: LatencyBudget) -> Self {
        self.latency_tracker = std::sync::Arc::new(LatencyBudgetTracker::new(budget));
        self
    }

    pub fn latency_tracker(&self) -> &LatencyBudgetTracker {
        &self.latency_tracker
    }

    #[instrument(
        name = "processor.process_transaction",
        skip(self),
        fields(
            transaction.id = %tx_id,
            trace_id = tracing::field::Empty,
            pipeline.queue_wait_ms = tracing::field::Empty
        )
    )]
    pub async fn process_transaction(&self, tx_id: uuid::Uuid) -> anyhow::Result<()> {
        // Fetch the transaction first
        let tx: crate::db::models::Transaction =
            sqlx::query_as("SELECT * FROM transactions WHERE id = $1")
                .bind(tx_id)
                .fetch_one(&self.pool)
                .await?;

        // Record the trace ID (propagated from the inbound webhook that
        // created this transaction, see `handlers/webhook.rs`) onto this
        // span so every `tracing` event emitted by the pipeline stages below
        // — validate/enrich/verify/complete — carries it structurally,
        // without threading it through each stage's function signature.
        if let Some(trace_id) = &tx.trace_id {
            tracing::Span::current().record("trace_id", &trace_id.as_str());
        }

        // Time the transaction sat pending between ingestion and now; the
        // latency-budget layer (telemetry::latency_budget) charges it to the
        // processing stage.
        let queue_wait_ms = (chrono::Utc::now() - tx.created_at)
            .num_milliseconds()
            .max(0);
        tracing::Span::current().record(
            crate::telemetry::latency_budget::QUEUE_WAIT_FIELD,
            queue_wait_ms,
        );

        // Define the pipeline stages
        let mut stages: Vec<Box<dyn ProcessingStage>> = Vec::new();

        // Validate stage - always enabled
        stages.push(Box::new(ValidateStage));

        // Enrich stage - feature flagged, gated per stellar_account so an
        // operator-configured rollout_percentage is actually respected
        // instead of silently applying to 100% of traffic the moment the
        // flag is merely enabled=true.
        if self
            .feature_flags
            .is_enabled_for_key("transaction_enrich_stage", &tx.stellar_account)
            .await
            .unwrap_or(false)
        {
            crate::metrics::transaction_processor_stage_executions_total()
                .add(1, &[opentelemetry::KeyValue::new("stage", "enrich")]);
            stages.push(Box::new(EnrichStage));
        }

        // Verify stage - feature flagged (see EnrichStage comment above)
        if self
            .feature_flags
            .is_enabled_for_key("transaction_verify_stage", &tx.stellar_account)
            .await
            .unwrap_or(false)
        {
            crate::metrics::transaction_processor_stage_executions_total()
                .add(1, &[opentelemetry::KeyValue::new("stage", "verify")]);
            stages.push(Box::new(VerifyStage));
        }

        // Complete stage - always enabled
        stages.push(Box::new(CompleteStage::new(
            self.pool.clone(),
            self.query_cache.clone(),
        )));

        // Execute the pipeline, attributing each stage's observed duration to
        // its latency-budget stage.
        for stage in stages {
            let stage_name = stage.name();
            let start = std::time::Instant::now();
            tracing::info!("Starting {} stage for transaction {}", stage_name, tx_id);

            // Span names feed telemetry::latency_budget: the validate stage
            // counts toward the validation budget, the rest toward processing.
            let stage_span = if stage_name == "validate" {
                tracing::info_span!("processor.stage.validate")
            } else {
                tracing::info_span!("processor.stage", stage = stage_name)
            };
            match stage.execute(&tx).instrument(stage_span).await {
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

        // Report actual per-stage latency against budget and alert when a
        // stage consistently exceeds its allotted share.
        let report = self.latency_tracker.report(&stage_spans);
        let mut consecutive: HashMap<LatencyStage, u32> = HashMap::new();
        self.latency_tracker
            .record_and_alert(&report, &mut consecutive, 3);

        Ok(())
    }
}
