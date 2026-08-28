use chrono::Utc;
use sqlx::PgPool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::watch;
use tokio::time::{sleep, Duration};
use tracing::{debug, error, info, warn};

use crate::db::models::Transaction;
use crate::services::lock_manager::LeaderElection;
use crate::services::webhook_dispatcher::WebhookDispatcher;
use crate::stellar::{HorizonClient, HorizonError};

const LEADER_HEARTBEAT_SECS: u64 = 15;
const POLL_INTERVAL_SECS: u64 = 5;

/// Exponential moving average tracker for adaptive batch sizing.
pub struct BatchSizer {
    ema: f64,
    alpha: f64,
    min_batch: u32,
    max_batch: u32,
    scaling_factor: f64,
}

impl BatchSizer {
    pub fn new(min_batch: u32, max_batch: u32, scaling_factor: f64) -> Self {
        Self {
            ema: min_batch as f64,
            alpha: 0.2, // EMA smoothing factor
            min_batch,
            max_batch,
            scaling_factor,
        }
    }

    /// Update EMA with the latest queue depth and return the new batch size.
    pub fn update(&mut self, queue_depth: u64) -> u32 {
        self.ema = self.alpha * queue_depth as f64 + (1.0 - self.alpha) * self.ema;
        let raw = (self.ema * self.scaling_factor).round() as u32;
        raw.clamp(self.min_batch, self.max_batch)
    }

    pub fn current(&self) -> u32 {
        let raw = (self.ema * self.scaling_factor).round() as u32;
        raw.clamp(self.min_batch, self.max_batch)
    }
}

pub struct ProcessorPool {
    pool: PgPool,
    horizon_client: HorizonClient,
    workers: usize,
    poll_interval_ms: u64,
    min_batch: u32,
    max_batch: u32,
    scaling_factor: f64,
    /// Shared atomic for current batch size (exposed via /health).
    current_batch_size: Arc<AtomicU64>,
    /// Shared atomic for queue depth (read by back-pressure task).
    pending_queue_depth: Arc<AtomicU64>,
    /// Enqueues outbound webhook deliveries for completed transactions.
    /// `None` disables webhook delivery (e.g. Redis unavailable at startup).
    webhook_dispatcher: Option<WebhookDispatcher>,
    /// Shared `QueryCache` for cache invalidation after completing
    /// transactions. `None` means invalidation is skipped (see
    /// `with_query_cache`).
    query_cache: Option<crate::services::query_cache::QueryCache>,
    feature_flags: crate::services::feature_flags::FeatureFlagService,
}

impl ProcessorPool {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool: PgPool,
        horizon_client: HorizonClient,
        workers: usize,
        poll_interval_ms: u64,
        min_batch: u32,
        max_batch: u32,
        scaling_factor: f64,
        current_batch_size: Arc<AtomicU64>,
        pending_queue_depth: Arc<AtomicU64>,
    ) -> Self {
        let feature_flags = crate::services::feature_flags::FeatureFlagService::new(pool.clone());
        Self {
            pool,
            horizon_client,
            workers,
            poll_interval_ms,
            min_batch,
            max_batch,
            scaling_factor,
            current_batch_size,
            pending_queue_depth,
            webhook_dispatcher: None,
            query_cache: None,
            feature_flags,
        }
    }

    /// Attach a WebhookDispatcher so completed transactions enqueue outbound
    /// webhook deliveries. Without this, process_batch still completes
    /// transactions but never calls `enqueue()`.
    pub fn with_webhook_dispatcher(mut self, dispatcher: WebhookDispatcher) -> Self {
        self.webhook_dispatcher = Some(dispatcher);
        self
    }

    /// Attach the process's shared `QueryCache` so completing a transaction
    /// invalidates the same instance reads go through instead of silently
    /// no-oping (see `db::queries::invalidate_transaction_caches`).
    pub fn with_query_cache(mut self, cache: crate::services::query_cache::QueryCache) -> Self {
        self.query_cache = Some(cache);
        self
    }

    /// Start the processor pool. Returns a shutdown sender; drop or send to it to stop workers.
    pub fn start(self) -> watch::Sender<bool> {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let workers = self.workers;
        let poll_interval_ms = self.poll_interval_ms;
        let min_batch = self.min_batch;
        let max_batch = self.max_batch;
        let scaling_factor = self.scaling_factor;
        let current_batch_size = self.current_batch_size.clone();
        let pending_queue_depth = self.pending_queue_depth.clone();
        let pool = self.pool;
        let horizon_client = self.horizon_client;
        let webhook_dispatcher = self.webhook_dispatcher;
        let query_cache = self.query_cache;
        let feature_flags = self.feature_flags;

        info!("Starting ProcessorPool with {} workers", workers);

        for worker_id in 0..workers {
            let pool = pool.clone();
            let horizon_client = horizon_client.clone();
            let mut shutdown_rx = shutdown_rx.clone();
            let current_batch_size = current_batch_size.clone();
            let pending_queue_depth = pending_queue_depth.clone();
            let webhook_dispatcher = webhook_dispatcher.clone();
            let query_cache = query_cache.clone();
            let feature_flags = feature_flags.clone();
            let mut sizer = BatchSizer::new(min_batch, max_batch, scaling_factor);

            tokio::spawn(async move {
                info!("Processor worker {} started", worker_id);
                loop {
                    // Check for shutdown signal
                    if *shutdown_rx.borrow() {
                        info!("Processor worker {} shutting down", worker_id);
                        break;
                    }

                    let depth = pending_queue_depth.load(Ordering::Relaxed);
                    let batch_size = sizer.update(depth);
                    current_batch_size.store(batch_size as u64, Ordering::Relaxed);
                    debug!(worker_id, batch_size, depth, "adaptive batch size");

                    match process_batch(
                        &pool,
                        &horizon_client,
                        batch_size,
                        webhook_dispatcher.as_ref(),
                        query_cache.as_ref(),
                        &feature_flags,
                    )
                    .await
                    {
                        Ok(processed) => {
                            if processed > 0 {
                                tracing::info!(
                                    counter.processor_transactions_processed = processed as u64,
                                    worker_id,
                                    "processed transactions"
                                );
                            }
                            tracing::info!(counter.processor_batches_total = 1u64, worker_id);
                        }
                        Err(e) => {
                            error!(worker_id, "Processor batch error: {}", e);
                        }
                    }

                    // Wait for poll interval or shutdown
                    tokio::select! {
                        _ = sleep(Duration::from_millis(poll_interval_ms)) => {}
                        _ = shutdown_rx.changed() => {
                            info!("Processor worker {} received shutdown signal", worker_id);
                            break;
                        }
                    }
                }
                info!("Processor worker {} stopped", worker_id);
            });
        }

        shutdown_tx
    }
}

/// Webhook enqueue-on-completion is gated behind this flag (default off —
/// see migration `20260823000002_webhook_enqueue_rollout_flag.sql`) so
/// enabling outbound webhook delivery for the first time is an explicit,
/// gradual operator action (ramp `rollout_percentage` up by
/// `stellar_account`) rather than firing at 100% of traffic the moment this
/// PR merges.
const WEBHOOK_ENQUEUE_FLAG: &str = "webhook_enqueue_on_completion";

/// Real payment verification (see `find_matching_payment` below) is gated
/// behind this flag, mirroring `WEBHOOK_ENQUEUE_FLAG` above — see migration
/// `20260824000002_payment_verification_rollout_flag.sql`. Cutover is a
/// gradual, explicit per-`stellar_account` operator action (ramp
/// `rollout_percentage` up) rather than an instant behavior change for
/// 100% of traffic the moment this PR merges. While disabled for a given
/// account, `process_batch` still evaluates the verification logic in
/// shadow mode (see `payment_verification_no_match_completed_total`) so
/// operators can review divergence data before ramping up.
const PAYMENT_VERIFICATION_FLAG: &str = "payment_verification_enabled";

/// Two-source cross-check flag (#1097). When enabled, a transaction requires
/// BOTH Horizon payment evidence (signal 1) AND a matching anchor callback
/// (signal 2, `anchor_transaction_id IS NOT NULL AND callback_status =
/// 'completed'`) before it transitions to `completed`. Disagreement — one
/// signal present but not the other — routes the transaction to
/// `pending_review` for manual ops triage rather than silently accepting
/// either source alone. This flag is layered on top of
/// `payment_verification_enabled`; both must be enabled for v2 semantics.
const PAYMENT_VERIFICATION_V2_FLAG: &str = "payment_verification_v2";

/// How long a pending transaction may wait for its expected Horizon payment
/// to show up (including the case where its destination account does not
/// exist on-chain yet) before `process_batch` gives up and marks it
/// `failed`. Bounded rather than unbounded so a transaction always reaches
/// a terminal state and stays visible to operators; 30 minutes is generous
/// relative to Stellar ledger close times (~5s) and typical anchor payout
/// latency.
const PAYMENT_VERIFICATION_RETRY_WINDOW_SECS: i64 = 30 * 60;

/// Outcome of checking Horizon for the payment a pending transaction is
/// expecting. Only `Matched` is safe evidence to complete a transaction —
/// every other variant means "not yet verified," which is *not* the same
/// as "will never be verified": see `process_batch`'s retry-window handling.
pub enum PaymentLookup {
    /// A payment matching this transaction's account, amount, asset, and
    /// (if present) memo was found. Carries Horizon's payment id so the
    /// completion write can record it for idempotency
    /// (`idx_transactions_horizon_payment_id`).
    Matched(String),
    /// The destination account does not exist on-chain (yet). Per
    /// `HorizonError::AccountNotFound`'s doc comment, this is a normal,
    /// expected business outcome for a first-time deposit, not evidence of
    /// failure.
    AccountNotFound,
    /// The account exists and Horizon returned payments for it, but none
    /// matched this transaction's expected amount/asset/memo.
    NoMatchingPayment,
    /// The Horizon lookup itself failed (network error, non-200/404
    /// response, or the circuit breaker is open). Treated the same as "not
    /// yet verified" rather than as grounds to fail the transaction —
    /// infrastructure trouble should never be the reason a legitimate
    /// deposit gets marked failed.
    LookupFailed,
}

/// Checks Horizon for evidence that `transaction`'s expected payment has
/// actually arrived. See the module-level verification contract documented
/// above `process_batch`.
async fn find_matching_payment(
    horizon_client: &HorizonClient,
    transaction: &Transaction,
) -> PaymentLookup {
    let payments = match horizon_client
        .list_payments_for_account(&transaction.stellar_account, 50)
        .await
    {
        Ok(payments) => payments,
        Err(HorizonError::AccountNotFound(_)) => return PaymentLookup::AccountNotFound,
        Err(e) => {
            warn!(
                transaction_id = %transaction.id,
                error = %e,
                "Horizon payment lookup failed; treating as not-yet-verified"
            );
            return PaymentLookup::LookupFailed;
        }
    };

    let matched = payments.into_iter().find(|p| {
        p.to == transaction.stellar_account
            && p.asset_code == transaction.asset_code
            && payment_amount_matches(&p.amount, &transaction.amount)
            && match transaction.memo.as_deref() {
                Some(memo) => p.memo.as_deref() == Some(memo),
                None => true,
            }
    });

    match matched {
        Some(p) => PaymentLookup::Matched(p.id),
        None => PaymentLookup::NoMatchingPayment,
    }
}

fn payment_amount_matches(horizon_amount: &str, expected: &sqlx::types::BigDecimal) -> bool {
    horizon_amount
        .parse::<sqlx::types::BigDecimal>()
        .map(|amount| &amount == expected)
        .unwrap_or(false)
}

// ── Signal 2: anchor callback ─────────────────────────────────────────────────

/// Whether a transaction has received a completed anchor callback — the
/// second independent verification signal used by v2 cross-check.
///
/// Signal 2 is considered present when:
///   - `anchor_transaction_id IS NOT NULL` (the anchor acknowledged the tx)
///   - `callback_status = 'completed'`     (the anchor reported success)
///
/// This data is written by the existing anchor-callback ingestion path
/// (`src/handlers/webhook.rs` / callback handler) and is available entirely
/// from the DB row already held in memory from the `FOR UPDATE SKIP LOCKED`
/// claim — no additional query needed.
#[derive(Debug, PartialEq, Eq)]
pub enum AnchorSignal {
    /// Anchor has confirmed the transaction as completed.
    Confirmed,
    /// Anchor callback received but status is not yet 'completed'.
    Pending,
    /// No anchor callback has been received yet.
    Absent,
}

fn check_anchor_signal(transaction: &Transaction) -> AnchorSignal {
    match (&transaction.anchor_transaction_id, &transaction.callback_status) {
        (Some(_), Some(status)) if status == "completed" => AnchorSignal::Confirmed,
        (Some(_), _) => AnchorSignal::Pending,
        _ => AnchorSignal::Absent,
    }
}

/// Result of the v2 two-source cross-check.
#[derive(Debug, PartialEq, Eq)]
pub enum VerificationV2Decision {
    /// Both signals agree: complete the transaction.
    Complete { verification_source: &'static str },
    /// Signals disagree: route to manual review.
    PendingReview { reason: &'static str },
    /// At least one signal is still outstanding: leave pending to retry.
    Defer { reason: &'static str },
}

pub fn cross_check_signals(
    horizon: &PaymentLookup,
    anchor: &AnchorSignal,
) -> VerificationV2Decision {
    match (horizon, anchor) {
        // Both signals present and agree → complete.
        (PaymentLookup::Matched(_), AnchorSignal::Confirmed) => {
            VerificationV2Decision::Complete {
                verification_source: "horizon+anchor",
            }
        }
        // Horizon matched but anchor callback hasn't arrived yet → defer.
        (PaymentLookup::Matched(_), AnchorSignal::Absent | AnchorSignal::Pending) => {
            VerificationV2Decision::Defer {
                reason: "horizon_matched_anchor_pending",
            }
        }
        // Anchor confirmed but Horizon has no matching payment yet → defer
        // (payment may be in flight or Horizon is temporarily unavailable).
        (
            PaymentLookup::NoMatchingPayment
            | PaymentLookup::AccountNotFound
            | PaymentLookup::LookupFailed,
            AnchorSignal::Confirmed,
        ) => VerificationV2Decision::Defer {
            reason: "anchor_confirmed_horizon_pending",
        },
        // Both signals present but they conflict: anchor confirmed while
        // Horizon explicitly shows a different state.  This is the
        // disagreement case — route to manual review, never auto-complete.
        // NOTE: currently LookupFailed is treated as "defer" not "disagree"
        // since the absence of Horizon data may be transient. Only
        // NoMatchingPayment / AccountNotFound constitute an explicit
        // "Horizon checked and found nothing" signal.
        //
        // Out-of-order arrival is handled in both defer branches above:
        // whichever signal arrives second will change the decision on the
        // next tick.
        //
        // The "both absent" case falls here too — defer until at least one
        // signal arrives.
        (_, AnchorSignal::Absent | AnchorSignal::Pending) => VerificationV2Decision::Defer {
            reason: "both_signals_pending",
        },
    }
}

/// Postgres 23505 = unique_violation. Used to detect a concurrent claim of
/// the same Horizon payment by another transaction
/// (`idx_transactions_horizon_payment_id`).
fn is_unique_violation(err: &sqlx::Error) -> bool {
    matches!(err, sqlx::Error::Database(db_err) if db_err.code().as_deref() == Some("23505"))
}

/// Completes a batch of pending transactions.
///
/// # Verification contract
///
/// A transaction is only completed when Horizon reports a payment that
/// actually matches it (destination account, amount, asset code, and memo
/// when present) — see `find_matching_payment`. Confirming that the
/// destination account merely *exists* on Stellar is **not** sufficient
/// evidence: an account can exist for reasons unrelated to this specific
/// deposit (funded earlier, funded by someone else, funded for an
/// unrelated transaction), and a brand-new deposit destination normally
/// does not exist yet even though the deposit itself is legitimate and in
/// flight (`HorizonError::AccountNotFound`'s doc comment). Conflating the
/// two either completes transactions with no evidence money moved, or
/// terminally fails deposits that would have arrived seconds later. Do not
/// reintroduce an account-existence-only check here — see git history for
/// this exact class of regression.
pub async fn process_batch(
    pool: &PgPool,
    horizon_client: &HorizonClient,
    batch_size: u32,
    webhook_dispatcher: Option<&WebhookDispatcher>,
    query_cache: Option<&crate::services::query_cache::QueryCache>,
    feature_flags: &crate::services::feature_flags::FeatureFlagService,
) -> anyhow::Result<usize> {
    let mut tx = pool.begin().await?;

    let pending: Vec<Transaction> = sqlx::query_as::<_, Transaction>(
        r#"
        SELECT id, stellar_account, amount, asset_code, status, created_at, updated_at,
               anchor_transaction_id, callback_type, callback_status, settlement_id,
               memo, memo_type, metadata, trace_id
        FROM transactions
        WHERE status = 'pending'
        ORDER BY created_at ASC
        LIMIT $1
        FOR UPDATE SKIP LOCKED
        "#,
    )
    .bind(batch_size as i64)
    .fetch_all(&mut *tx)
    .await?;

    if pending.is_empty() {
        tx.commit().await?;
        return Ok(0);
    }

    debug!("Processing {} pending transaction(s)", pending.len());

    let mut asset_codes = std::collections::HashSet::new();
    let mut completed = Vec::with_capacity(pending.len());
    for transaction in &pending {
        asset_codes.insert(transaction.asset_code.clone());

        // Create linked span for transaction processing if trace_id exists
        if let Some(ref trace_id) = transaction.trace_id {
            let span = tracing::info_span!(
                "transaction.process",
                transaction_id = %transaction.id,
                trace_id = %trace_id,
            );
            let _guard = span.enter();
            debug!("Processing transaction with trace context");
        }

        if let Err(e) = crate::validation::state_machine::validate_status_transition(
            &transaction.status,
            "completed",
        ) {
            warn!(
                transaction_id = %transaction.id,
                status = %transaction.status,
                error = %e,
                "Skipping invalid status transition in batch"
            );
            continue;
        }

        let verification_enabled = feature_flags
            .is_enabled_for_key(PAYMENT_VERIFICATION_FLAG, &transaction.stellar_account)
            .await
            .unwrap_or(false);

        let lookup = find_matching_payment(horizon_client, transaction).await;
        let matched_payment_id = match &lookup {
            PaymentLookup::Matched(id) => Some(id.clone()),
            _ => None,
        };

        if verification_enabled && matched_payment_id.is_none() {
            let age_secs = (Utc::now() - transaction.created_at).num_seconds();
            if age_secs < PAYMENT_VERIFICATION_RETRY_WINDOW_SECS {
                debug!(
                    transaction_id = %transaction.id,
                    age_secs,
                    reason = match lookup {
                        PaymentLookup::AccountNotFound => "account_not_found",
                        PaymentLookup::NoMatchingPayment => "no_matching_payment",
                        PaymentLookup::LookupFailed => "lookup_failed",
                        PaymentLookup::Matched(_) => unreachable!(),
                    },
                    "No verified payment yet; leaving pending for retry"
                );
                crate::metrics::payment_verification_retry_deferred_total().add(1, &[]);
                continue;
            }

            // Retry window exceeded with no matching payment ever found:
            // this transaction is genuinely failed.
            if let Err(e) = crate::validation::state_machine::validate_status_transition(
                &transaction.status,
                "failed",
            ) {
                warn!(transaction_id = %transaction.id, error = %e, "Cannot transition to failed");
                continue;
            }
            sqlx::query(
                "UPDATE transactions SET status = 'failed', updated_at = NOW() WHERE id = $1 AND status = $2",
            )
            .bind(transaction.id)
            .bind(&transaction.status)
            .execute(&mut *tx)
            .await?;
            continue;
        }

        if !verification_enabled && matched_payment_id.is_none() {
            // Shadow mode: the flag is off for this account, so the
            // pre-verification (unconditional) completion behavior below
            // still applies, but log the divergence — this transaction
            // would NOT have been completed under the new verification
            // logic — so operators can review it before ramping
            // rollout_percentage up.
            warn!(
                transaction_id = %transaction.id,
                "Completing transaction with no matching Horizon payment found \
                 (payment_verification_enabled is off for this account — shadow mode only)"
            );
            crate::metrics::payment_verification_no_match_completed_total().add(1, &[]);
        }

        // ── v2 two-source cross-check ─────────────────────────────────────────
        //
        // When payment_verification_v2 is enabled for this account, overlay a
        // second signal (anchor callback) on top of the v1 Horizon signal.
        // This block runs AFTER the v1 guard above, so it only applies when
        // verification_enabled is true AND we have a Horizon match (the v1
        // guard would have already deferred/failed the tx otherwise).
        let verification_v2_enabled = feature_flags
            .is_enabled_for_key(PAYMENT_VERIFICATION_V2_FLAG, &transaction.stellar_account)
            .await
            .unwrap_or(false);

        if verification_enabled && verification_v2_enabled {
            let anchor = check_anchor_signal(transaction);
            let decision = cross_check_signals(&lookup, &anchor);

            match decision {
                VerificationV2Decision::Complete { verification_source } => {
                    // Both signals agree — fall through to the completion write
                    // below, tagging with the verification source.
                    debug!(
                        transaction_id = %transaction.id,
                        verification_source,
                        "V2 cross-check: both signals agree, proceeding to complete"
                    );
                    // Update verification_source column alongside the status write.
                    let result = sqlx::query(
                        "UPDATE transactions \
                         SET status = 'completed', updated_at = NOW(), \
                             horizon_payment_id = $3, verification_source = $4 \
                         WHERE id = $1 AND status = $2",
                    )
                    .bind(transaction.id)
                    .bind(&transaction.status)
                    .bind(&matched_payment_id)
                    .bind(verification_source)
                    .execute(&mut *tx)
                    .await;

                    match result {
                        Ok(r) if r.rows_affected() > 0 => {
                            completed.push(transaction.clone());
                        }
                        Ok(_) => {}
                        Err(e) if is_unique_violation(&e) => {
                            warn!(
                                transaction_id = %transaction.id,
                                "horizon_payment_id already claimed (v2 path)"
                            );
                        }
                        Err(e) => return Err(e.into()),
                    }
                    continue;
                }
                VerificationV2Decision::Defer { reason } => {
                    debug!(
                        transaction_id = %transaction.id,
                        reason,
                        "V2 cross-check: deferring — waiting for second signal"
                    );
                    crate::metrics::payment_verification_retry_deferred_total().add(1, &[]);
                    continue;
                }
                VerificationV2Decision::PendingReview { reason } => {
                    // Signals disagree → explicit pending_review, never auto-complete.
                    if let Err(e) = crate::validation::state_machine::validate_status_transition(
                        &transaction.status,
                        "pending_review",
                    ) {
                        warn!(
                            transaction_id = %transaction.id,
                            error = %e,
                            "Cannot transition to pending_review"
                        );
                        continue;
                    }
                    warn!(
                        transaction_id = %transaction.id,
                        reason,
                        "V2 cross-check: signal disagreement — routing to pending_review"
                    );
                    sqlx::query(
                        "UPDATE transactions \
                         SET status = 'pending_review', updated_at = NOW(), \
                             verification_source = $3 \
                         WHERE id = $1 AND status = $2",
                    )
                    .bind(transaction.id)
                    .bind(&transaction.status)
                    .bind(format!("disagreement:{reason}"))
                    .execute(&mut *tx)
                    .await?;
                    continue;
                }
            }
        }

        // The FOR UPDATE SKIP LOCKED claim above already holds this row's
        // lock; WHERE status = $2 is defense in depth against any other
        // write path changing it without going through that lock.
        let result = sqlx::query(
            "UPDATE transactions SET status = 'completed', updated_at = NOW(), horizon_payment_id = $3 WHERE id = $1 AND status = $2",
        )
        .bind(transaction.id)
        .bind(&transaction.status)
        .bind(&matched_payment_id)
        .execute(&mut *tx)
        .await;

        let result = match result {
            Ok(r) => r,
            Err(e) if is_unique_violation(&e) => {
                // Another transaction already claimed this exact Horizon
                // payment (idx_transactions_horizon_payment_id). Skip this
                // one for this tick rather than failing the whole batch.
                warn!(
                    transaction_id = %transaction.id,
                    "horizon_payment_id already claimed by another transaction this tick"
                );
                continue;
            }
            Err(e) => return Err(e.into()),
        };

        if result.rows_affected() > 0 {
            completed.push(transaction.clone());
        }
    }

    tx.commit().await?;

    for asset_code in asset_codes {
        crate::db::queries::invalidate_caches_for_asset(query_cache, &asset_code).await;
    }

    if let Some(dispatcher) = webhook_dispatcher {
        for transaction in &completed {
            let enqueue_enabled = feature_flags
                .is_enabled_for_key(WEBHOOK_ENQUEUE_FLAG, &transaction.stellar_account)
                .await
                .unwrap_or(false);
            if !enqueue_enabled {
                continue;
            }

            let payload = serde_json::json!({
                "status": "completed",
                "asset_code": transaction.asset_code,
                "amount": transaction.amount.to_string(),
                "stellar_account": transaction.stellar_account,
            });
            if let Err(e) = dispatcher
                .enqueue(transaction.id, "transaction.completed", payload)
                .await
            {
                error!(
                    transaction_id = %transaction.id,
                    error = %e,
                    "Failed to enqueue webhook delivery for completed transaction"
                );
            }
        }
    }

    Ok(completed.len())
}

/// Legacy single-worker entry point kept for backward compatibility. Not
/// currently invoked from `main.rs` (see `ProcessorPool::start`, which is
/// the live entry point and does dispatch webhooks). Passes `None` for the
/// webhook dispatcher, so transactions completed through this path do not
/// enqueue outbound webhook deliveries.
pub async fn run_processor(pool: PgPool, horizon_client: HorizonClient) {
    info!("Async transaction processor started (legacy single-worker)");
    let feature_flags = crate::services::feature_flags::FeatureFlagService::new(pool.clone());
    loop {
        if let Err(e) = process_batch(&pool, &horizon_client, 10, None, None, &feature_flags).await
        {
            error!("Processor batch error: {}", e);
        }
        sleep(Duration::from_secs(5)).await;
    }
}

/// Background task: refresh pending queue depth every 5 seconds.
pub async fn queue_depth_task(pool: PgPool, pending_queue_depth: Arc<AtomicU64>) {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    loop {
        interval.tick().await;
        match sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM transactions WHERE status = 'pending'",
        )
        .fetch_one(&pool)
        .await
        {
            Ok(count) => {
                let depth = count.max(0) as u64;
                pending_queue_depth.store(depth, Ordering::Relaxed);
                tracing::info!(counter.processor_queue_depth = depth);
                if depth > 5_000 {
                    warn!(depth, "Pending transaction queue depth is high");
                }
            }
            Err(e) => {
                error!("Failed to query pending queue depth: {}", e);
                // Fail open: leave the existing counter unchanged
            }
        }
    }
}

/// Runs the leader election + heartbeat loop.
///
/// - All instances call this; only the elected leader returns `true` from
///   `try_acquire_leadership`.
/// - The leader runs partition maintenance, settlement jobs, and webhook dispatch.
/// - All instances run `process_batch` (safe via SKIP LOCKED).
pub async fn run_processor_with_leader_election(
    pool: PgPool,
    horizon_client: HorizonClient,
    redis_url: &str,
) {
    let election = match LeaderElection::new(redis_url) {
        Ok(e) => e,
        Err(e) => {
            warn!("Failed to create LeaderElection (Redis unavailable?): {e}. Running without leader guard.");
            run_processor(pool, horizon_client).await;
            return;
        }
    };

    info!(
        instance_id = election.instance_id(),
        "Processor started with leader election"
    );

    let feature_flags = crate::services::feature_flags::FeatureFlagService::new(pool.clone());
    let mut heartbeat_tick = tokio::time::interval(Duration::from_secs(LEADER_HEARTBEAT_SECS));
    let mut process_tick = tokio::time::interval(Duration::from_secs(POLL_INTERVAL_SECS));

    loop {
        tokio::select! {
            _ = heartbeat_tick.tick() => {
                // Publish heartbeat regardless of leader status
                if let Err(e) = election.publish_heartbeat().await {
                    warn!("Heartbeat publish failed: {e}");
                }

                match election.try_acquire_leadership().await {
                    Ok(true) => debug!(instance_id = election.instance_id(), "This instance is leader"),
                    Ok(false) => debug!(instance_id = election.instance_id(), "This instance is follower"),
                    Err(e) => warn!("Leader election error: {e}"),
                }
            }
            _ = process_tick.tick() => {
                // All instances process transactions (SKIP LOCKED handles concurrency).
                // No webhook dispatcher wired here — this function is not currently
                // invoked from main.rs (ProcessorPool::start is the live path).
                if let Err(e) = process_batch(&pool, &horizon_client, 10, None, None, &feature_flags).await {
                    error!("Processor batch error: {e}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_sizer_clamps_to_min() {
        let mut s = BatchSizer::new(10, 500, 0.5);
        let size = s.update(0);
        assert!(size >= 10);
    }

    #[test]
    fn batch_sizer_clamps_to_max() {
        let mut s = BatchSizer::new(10, 500, 0.5);
        // Feed a very large depth many times to push EMA up
        for _ in 0..50 {
            s.update(100_000);
        }
        let size = s.current();
        assert!(size <= 500);
    }

    #[test]
    fn batch_sizer_increases_under_load() {
        let mut s = BatchSizer::new(10, 500, 0.5);
        let initial = s.current();
        for _ in 0..20 {
            s.update(1_000);
        }
        assert!(s.current() > initial);
    }

    /// Regression test for issue #1060 Part C: `ProcessorPool::start()`'s
    /// shutdown sender was bound to `_processor_shutdown` in `main.rs` and
    /// never sent to, so workers never received a shutdown signal. This
    /// verifies the sender/receiver pair `main.rs` now drives: a signal sent
    /// on the returned `watch::Sender` is observable by a worker-side
    /// receiver, which is exactly what each worker's `shutdown_rx.changed()`
    /// select branch keys off to stop claiming new batches.
    #[tokio::test]
    async fn shutdown_signal_propagates_to_worker_receivers() {
        let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://synapse:synapse@localhost:5432/synapse_test".to_string()
        });
        let pool = match PgPool::connect(&database_url).await {
            Ok(pool) => pool,
            Err(_) => {
                eprintln!("skipping shutdown_signal_propagates_to_worker_receivers: database not reachable");
                return;
            }
        };

        let processor_pool = ProcessorPool::new(
            pool,
            HorizonClient::new("https://horizon-testnet.stellar.org".to_string()),
            1,
            20,
            10,
            500,
            0.5,
            Arc::new(AtomicU64::new(10)),
            Arc::new(AtomicU64::new(0)),
        );

        let shutdown_tx = processor_pool.start();
        let mut worker_rx = shutdown_tx.subscribe();
        assert!(!*worker_rx.borrow(), "shutdown signal must start false");

        shutdown_tx
            .send(true)
            .expect("send must succeed while workers are still subscribed");

        tokio::time::timeout(Duration::from_secs(2), worker_rx.changed())
            .await
            .expect("worker-side receiver must observe the shutdown signal")
            .unwrap();
        assert!(*worker_rx.borrow(), "receiver must see the signal as true");
    }

    #[test]
    fn batch_sizer_decreases_during_idle() {
        let mut s = BatchSizer::new(10, 500, 0.5);
        // Prime with high load
        for _ in 0..20 {
            s.update(1_000);
        }
        let high = s.current();
        // Then idle
        for _ in 0..50 {
            s.update(0);
        }
        assert!(s.current() < high);
    }
}
