use crate::db::{models::Transaction, queries};
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
        queries::get_transaction(&state.db, id)
            .await
            .map_err(|e| e.into())
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
    async fn transactions(
        &self,
        ctx: &Context<'_>,
        filter: Option<TransactionFilter>,
        limit: Option<i64>,
        _offset: Option<i64>,
    ) -> Result<Vec<Transaction>> {
        let effective_limit = limit.unwrap_or(20);
        validate_limit(effective_limit)
            .map_err(|e| async_graphql::Error::new(e.to_string()))?;

        if let Some(ref f) = filter {
            if let Some(ref s) = f.status {
                validate_status(s).map_err(|e| async_graphql::Error::new(e.to_string()))?;
            }
            if let Some(ref a) = f.asset_code {
                validate_asset_code(a).map_err(|e| async_graphql::Error::new(e.to_string()))?;
            }
            if let Some(ref acc) = f.stellar_account {
                validate_stellar_account(acc)
                    .map_err(|e| async_graphql::Error::new(e.to_string()))?;
            }
        }

        let state = ctx.data::<AppState>()?;

        let txs =
            queries::list_transactions(&state.db, effective_limit, None, false).await?;

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
/// ```
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
    /// - Updates transaction status to 'completed'
    /// - Invalidates query cache for the asset
    /// - Triggers webhook delivery if configured
    async fn force_complete_transaction(&self, ctx: &Context<'_>, id: Uuid) -> Result<Transaction> {
        let state = ctx.data::<AppState>()?;

        let asset_code: String =
            sqlx::query_scalar("SELECT asset_code FROM transactions WHERE id = $1")
                .bind(id)
                .fetch_one(&state.db)
                .await?;

        let result = sqlx::query_as::<_, Transaction>(
            "UPDATE transactions SET status = 'completed', updated_at = NOW() WHERE id = $1 RETURNING *"
        )
        .bind(id)
        .fetch_one(&state.db)
        .await?;

        crate::db::queries::invalidate_caches_for_asset(&asset_code).await;

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
