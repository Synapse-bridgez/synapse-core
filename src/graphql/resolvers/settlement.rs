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
        let settlements =
            queries::list_settlements(&state.db, limit.unwrap_or(20), offset.unwrap_or(0))
                .await
                .map_err(|e| e.into())?;

        // Batch the related-data lookups for the whole page in a single
        // query instead of issuing one query per settlement (N+1). The
        // returned shape is unchanged; only the number of queries is
        // bounded, which is observable via the query counter in
        // `src/metrics.rs`.
        let ids: Vec<i64> = settlements.iter().map(|s| s.id).collect();
        let related = queries::get_settlements_related_batch(&state.db, &ids)
            .await
            .map_err(|e| e.into())?;

        Ok(settlements
            .into_iter()
            .map(|mut settlement| {
                if let Some(extra) = related.get(&settlement.id) {
                    settlement.related = extra.clone();
                }
                settlement
            })
            .collect())
    }
}
