use async_graphql::{Object, Context, Result};
use crate::AppState;
use crate::db::{models::Settlement, queries};

#[derive(Default)]
pub struct SettlementQuery;

#[Object]
impl SettlementQuery {
    async fn settlements(&self, ctx: &Context<'_>, limit: Option<i64>, offset: Option<i64>) -> Result<Vec<Settlement>> {
        let state = ctx.data::<AppState>()?;
        queries::list_settlements(&state.db, limit.unwrap_or(20), offset.unwrap_or(0)).await
            .map_err(|e| e.into())
    }
}
