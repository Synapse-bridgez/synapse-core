use crate::db::{models::Settlement, queries};
use crate::AppState;
use async_graphql::{Context, Object, Result};

#[derive(Default)]
pub struct SettlementQuery;

#[Object]
impl SettlementQuery {
    /// Cost scales with the requested `limit` (default 20) — see the
    /// equivalent doc comment on `TransactionQuery::transactions` for why.
    #[graphql(complexity = "limit.unwrap_or(20).max(1) as usize + child_complexity")]
    async fn settlements(
        &self,
        ctx: &Context<'_>,
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> Result<Vec<Settlement>> {
        let state = ctx.data::<AppState>()?;
        queries::list_settlements(&state.db, limit.unwrap_or(20), offset.unwrap_or(0))
            .await
            .map_err(|e| e.into())
    }
}
