use crate::db::{models::Transaction, queries};
use crate::error::AppError;
use crate::graphql::error::{GqlResultExt, IntoGraphQlError};
use crate::graphql::input_validation::{
    validate_asset_code, validate_limit, validate_status, validate_stellar_account,
};
use crate::handlers::ws::TransactionStatusUpdate;
use crate::AppState;
use async_graphql::{Context, InputObject, Object, Result, Subscription};
use futures::Stream;
use std::pin::Pin;
use tokio_stream::StreamExt as _;
use uuid::Uuid;

/// Filter criteria for transaction queries.
///
/// All fields are optional and combined with AND logic.
#[derive(InputObject)]
pub struct TransactionFilter {
    pub status: Option<String>,
    pub asset_code: Option<String>,
    pub stellar_account: Option<String>,
}

/// Transaction query resolver.
///
/// # Tenant scoping
///
/// Deliberately *not* tenant-filtered, unlike the REST `/transactions`
/// routes (`queries::get_transaction_for_tenant` /
/// `TenantContext`-scoped listing). `/graphql` is mounted only behind
/// `admin_auth` (`middleware/auth.rs`), which checks a single shared
/// platform-admin secret — not a per-tenant credential — so, same as every
/// other route already in `admin_router` (settlement dispute review,
/// reconciliation reports), a caller who clears that gate is a full
/// platform admin by design and is supposed to see across all tenants.
/// Adding tenant filtering here would not close an authorization gap; it
/// would break legitimate admin functionality. If a tenant-scoped-admin
/// role is ever introduced, this resolver needs revisiting then.
///
/// # Idempotency
///
/// Query operations are inherently idempotent and do not require
/// `X-Idempotency-Key` headers. Only mutations require idempotency keys.
#[derive(Default)]
pub struct TransactionQuery;

#[Object]
impl TransactionQuery {
    /// Fetch a single transaction by ID.
    ///
    /// # Arguments
    ///
    /// * `id` - The transaction UUID
    ///
    /// # Returns
    ///
    /// The transaction object or an error if not found.
    async fn transaction(&self, ctx: &Context<'_>, id: Uuid) -> Result<Transaction> {
        let state = ctx.data::<AppState>()?;
        queries::get_transaction(&state.db, id).await.into_gql()
    }

    /// List transactions with optional filtering.
    ///
    /// # Arguments
    ///
    /// * `filter` - Optional filter criteria (status, asset_code, stellar_account)
    /// * `limit` - Maximum number of results (default: 20)
    /// * `offset` - Pagination offset (default: 0)
    ///
    /// # Returns
    ///
    /// A vector of transactions matching the criteria.
    ///
    /// # Complexity
    ///
    /// Cost scales with the requested `limit` (default 20) rather than the
    /// field-occurrence default of 1, so `MAX_QUERY_COMPLEXITY` reflects the
    /// real row-fetch cost. Without this, `AliasLimitExtension`'s 20-alias cap
    /// still permits up to `MAX_QUERY_LIMIT` (1000) rows per alias — a single
    /// query could request 20,000 rows while staying "cheap" under the old
    /// per-field accounting.
    #[graphql(complexity = "limit.unwrap_or(20).max(1) as usize + child_complexity")]
    async fn transactions(
        &self,
        ctx: &Context<'_>,
        filter: Option<TransactionFilter>,
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> Result<Vec<Transaction>> {
        let effective_limit = limit.unwrap_or(20);
        validate_limit(effective_limit).into_gql()?;

        if let Some(ref f) = filter {
            if let Some(ref s) = f.status {
                validate_status(s).into_gql()?;
            }
            if let Some(ref a) = f.asset_code {
                validate_asset_code(a).into_gql()?;
            }
            if let Some(ref acc) = f.stellar_account {
                validate_stellar_account(acc).into_gql()?;
            }
        }

        let _ = offset;
        let state = ctx.data::<AppState>()?;

        let txs = queries::list_transactions(&state.db, effective_limit, None, false)
            .await
            .into_gql()?;

        if let Some(f) = filter {
            let filtered = txs
                .into_iter()
                .filter(|t| {
                    let status_match = f.status.as_ref().map(|s| &t.status == s).unwrap_or(true);
                    let asset_match = f
                        .asset_code
                        .as_ref()
                        .map(|a| &t.asset_code == a)
                        .unwrap_or(true);
                    let account_match = f
                        .stellar_account
                        .as_ref()
                        .map(|acc| &t.stellar_account == acc)
                        .unwrap_or(true);
                    status_match && asset_match && account_match
                })
                .collect();
            Ok(filtered)
        } else {
            Ok(txs)
        }
    }
}

/// Transaction mutation resolver.
///
/// # Idempotency
///
/// All mutations in this resolver require an `X-Idempotency-Key` header
/// to ensure safe retries. The header value should be a stable, unique
/// identifier for the operation (e.g., transaction ID or request ID).
///
/// Example:
/// ```text
/// X-Idempotency-Key: 550e8400-e29b-41d4-a716-446655440000
/// ```
///
/// See [GraphQL Idempotency Documentation](../docs/graphql-idempotency.md)
/// for detailed information.
#[derive(Default)]
pub struct TransactionMutation;

#[Object]
impl TransactionMutation {
    /// Force complete a transaction.
    ///
    /// # Arguments
    ///
    /// * `id` - The transaction UUID to complete
    ///
    /// # Returns
    ///
    /// The updated transaction object.
    ///
    /// # Idempotency
    ///
    /// This mutation requires an `X-Idempotency-Key` header.
    /// Retrying with the same key will return the cached result
    /// without re-executing the mutation.
    ///
    /// # Side Effects
    ///
    /// - Validates the current status can legally transition to 'completed'
    ///   (see `validation::state_machine`) — an already-failed, refunded,
    ///   disputed, or mid-processing transaction is rejected rather than
    ///   silently force-completed.
    /// - Updates transaction status to 'completed' with a CAS-guarded write
    ///   (`WHERE status = <status just read>`), so two concurrent callers
    ///   racing this mutation cannot both "succeed": the loser's write
    ///   affects zero rows and the mutation returns an error instead.
    /// - Records an audit log entry for the status change.
    /// - Invalidates query cache for the asset.
    ///
    /// Part D fix: this previously ran an unconditional `UPDATE ... SET
    /// status = 'completed'` with no read of the current status and no
    /// `WHERE status = ...` guard — any admin-key holder could force *any*
    /// transaction, in any state, to completed, and two concurrent calls
    /// could both apparently succeed with whichever write landed last
    /// silently winning. See `services/transaction_processor.rs`'s
    /// `CompleteStage` for the equivalent guard on the batch-completion path.
    async fn force_complete_transaction(&self, ctx: &Context<'_>, id: Uuid) -> Result<Transaction> {
        let state = ctx.data::<AppState>()?;

        let mut db_tx = state.db.begin().await.into_gql()?;

        let (current_status, trace_id): (String, Option<String>) = sqlx::query_as(
            "SELECT status, trace_id FROM transactions WHERE id = $1 FOR UPDATE",
        )
        .bind(id)
        .fetch_one(&mut *db_tx)
        .await
        .into_gql()?;

        // validate_status_transition treats same-state transitions as
        // idempotently valid (by design, for callers that want retries to
        // no-op rather than error). That's wrong for *this* mutation
        // specifically: if a concurrent caller already completed this
        // transaction while we were blocked on the row lock above, we must
        // report a loss, not a silent no-op "success" — otherwise two
        // racing calls both return as if they'd completed it, which is
        // exactly the double-success this CAS guard exists to prevent.
        if current_status == "completed" {
            return Err(AppError::ConcurrentModification(
                "transaction was already completed by a concurrent request".to_string(),
            )
            .into_graphql_error());
        }

        crate::validation::state_machine::validate_status_transition(&current_status, "completed")
            .into_gql()?;

        let result = sqlx::query_as::<_, Transaction>(
            "UPDATE transactions SET status = 'completed', updated_at = NOW() \
             WHERE id = $1 AND status = $2 RETURNING *",
        )
        .bind(id)
        .bind(&current_status)
        .fetch_optional(&mut *db_tx)
        .await
        .into_gql()?;

        let result = match result {
            Some(t) => t,
            None => {
                return Err(AppError::ConcurrentModification(
                    "transaction status changed before completion could be applied".to_string(),
                )
                .into_graphql_error());
            }
        };

        // admin_auth is a single shared platform-admin secret today, not a
        // per-operator identity (see middleware/auth.rs::is_valid_admin_request)
        // — "admin" is the most specific actor available until that changes.
        crate::db::audit::AuditLog::log_status_change_traced(
            &mut db_tx,
            id,
            crate::db::audit::ENTITY_TRANSACTION,
            &current_status,
            "completed",
            "admin",
            trace_id.as_deref(),
        )
        .await
        .into_gql()?;

        db_tx.commit().await.into_gql()?;

        crate::db::queries::invalidate_caches_for_asset(
            Some(&state.query_cache),
            &result.asset_code,
        )
        .await;

        Ok(result)
    }

    /// Replay a transaction from the dead letter queue.
    ///
    /// # Arguments
    ///
    /// * `id` - The transaction UUID to replay
    ///
    /// # Returns
    ///
    /// `true` if replay was successful, `false` otherwise.
    ///
    /// # Idempotency
    ///
    /// This mutation requires an `X-Idempotency-Key` header.
    /// Retrying with the same key will return the cached result.
    async fn replay_dlq(&self, _ctx: &Context<'_>, id: Uuid) -> Result<bool> {
        tracing::info!("Replaying DLQ for ID: {}", id);
        Ok(true)
    }
}

/// Transaction subscription resolver.
///
/// # Idempotency
///
/// Subscriptions do not require idempotency keys as they are
/// long-lived connections that stream updates.
#[derive(Default)]
pub struct TransactionSubscription;

#[Subscription]
impl TransactionSubscription {
    /// Subscribe to real-time transaction status changes.
    /// Optionally filter by `transaction_id`, `tenant_id`, or `asset_code`.
    async fn transaction_status_changed(
        &self,
        ctx: &Context<'_>,
        transaction_id: Option<Uuid>,
        asset_code: Option<String>,
    ) -> Result<Pin<Box<dyn Stream<Item = TransactionStatusUpdate> + Send>>> {
        let state = ctx.data::<AppState>()?;
        let rx = state.tx_broadcast.subscribe();

        let stream = tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(move |result| {
            match result {
                Ok(update) => {
                    // Apply optional filters
                    let id_match = transaction_id
                        .map(|id| update.transaction_id == id)
                        .unwrap_or(true);
                    let asset_match = asset_code
                        .as_deref()
                        .map(|a| update.message.as_deref() == Some(a))
                        .unwrap_or(true);
                    if id_match && asset_match {
                        Some(update)
                    } else {
                        None
                    }
                }
                Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(n)) => {
                    tracing::warn!("GraphQL subscription lagged by {} messages", n);
                    None
                }
            }
        });

        Ok(Box::pin(stream))
    }
}
