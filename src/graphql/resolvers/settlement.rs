use crate::db::{models::Settlement, queries};
use crate::graphql::error::database_error;
use crate::graphql::pagination::OffsetPagination;
use crate::AppState;
use async_graphql::{Context, Object, Result};

#[derive(Default)]
pub struct SettlementQuery;

#[Object]
impl SettlementQuery {
    /// List settlements with optional offset-based pagination.
    ///
    /// `limit` is clamped to `[1, MAX_PAGE_SIZE]` (100). `offset` is clamped
    /// to `>= 0`. Raw database errors are redacted before being surfaced to
    /// the caller — use `extensions.code` to distinguish `DATABASE_ERROR`
    /// responses from application-level errors (issues #968, #969, #970).
    async fn settlements(
        &self,
        ctx: &Context<'_>,
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> Result<Vec<Settlement>> {
        // OffsetPagination validates and clamps both limit and offset so no
        // unbounded DB query can be issued by passing an arbitrary large limit
        // or a negative offset (issue #969 / #970).
        let pagination = OffsetPagination::new(offset, limit);

        let state = ctx.data::<AppState>()?;
        queries::list_settlements(&state.db, pagination.sql_limit(), pagination.sql_offset())
            .await
            // Use the dedicated database_error helper so the raw sqlx error
            // (which may contain column/table names or constraint details) is
            // logged server-side and a generic DATABASE_ERROR code is returned
            // to the caller instead (issue #968).
            .map_err(|e| database_error(&e))
    }
}
