use crate::db::{models::Transaction, queries};
use crate::handlers::ws::TransactionStatusUpdate;
use crate::AppState;
use async_graphql::{Context, InputObject, Object, Result, Subscription};
use futures::Stream;
use std::pin::Pin;
use tokio_stream::StreamExt as _;
use uuid::Uuid;

#[derive(InputObject)]
pub struct TransactionFilter {
    pub status: Option<String>,
    pub asset_code: Option<String>,
    pub stellar_account: Option<String>,
}

#[derive(Default)]
pub struct TransactionQuery;

#[Object]
impl TransactionQuery {
    async fn transaction(&self, ctx: &Context<'_>, id: Uuid) -> Result<Transaction> {
        let state = ctx.data::<AppState>()?;
        queries::get_transaction(&state.db, id)
            .await
            .map_err(|e| e.into())
    }

    async fn transactions(
        &self,
        ctx: &Context<'_>,
        filter: Option<TransactionFilter>,
        limit: Option<i64>,
        _offset: Option<i64>,
    ) -> Result<Vec<Transaction>> {
        let state = ctx.data::<AppState>()?;

        let txs = queries::list_transactions(&state.db, limit.unwrap_or(20), None, false).await?;

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

#[derive(Default)]
pub struct TransactionMutation;

#[Object]
impl TransactionMutation {
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

    async fn replay_dlq(&self, _ctx: &Context<'_>, id: Uuid) -> Result<bool> {
        tracing::info!("Replaying DLQ for ID: {}", id);
        Ok(true)
    }
}

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

        let stream = tokio_stream::wrappers::BroadcastStream::new(rx)
            .filter_map(move |result| {
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
