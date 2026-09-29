use crate::db::{models::Settlement, queries};
use crate::error::AppError;
use crate::graphql::error::GqlResultExt;
use crate::AppState;
use async_graphql::{Context, Object, Result};
use bigdecimal::BigDecimal;
use chrono::Utc;
use uuid::Uuid;

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
            .into_gql()
    }
}

#[derive(Default)]
pub struct SettlementMutation;

#[Object]
impl SettlementMutation {
    /// Open a dispute on a settlement.
    /// Transitions a settlement from 'completed' to 'pending_review'.
    ///
    /// # Arguments
    /// * `id` - The settlement UUID to dispute
    /// * `reason` - The reason for the dispute
    ///
    /// # Returns
    /// The updated settlement object
    async fn open_settlement_dispute(
        &self,
        ctx: &Context<'_>,
        id: Uuid,
        reason: String,
    ) -> Result<Settlement> {
        let state = ctx.data::<AppState>()?;
        let actor = "gql-admin"; // In a real implementation, extract from auth context

        // Use the shared update_settlement_status query (same as REST handler)
        queries::update_settlement_status(
            &state.db,
            id,
            "completed",
            "pending_review",
            Some(&reason),
            None,
            actor,
        )
        .await
        .map_err(|e| {
            if matches!(e, sqlx::Error::RowNotFound) {
                AppError::StaleTransition
            } else {
                AppError::DatabaseError(e.to_string())
            }
        })
        .into_gql()
    }

    /// Add a note to a disputed settlement (for operational tracking).
    /// This transitions a settlement from 'pending_review' to 'disputed'.
    ///
    /// # Arguments
    /// * `id` - The settlement UUID
    /// * `note` - The note/reason to add
    ///
    /// # Returns
    /// The updated settlement object
    async fn add_dispute_note(
        &self,
        ctx: &Context<'_>,
        id: Uuid,
        note: String,
    ) -> Result<Settlement> {
        let state = ctx.data::<AppState>()?;
        let actor = "gql-admin";

        // Transition from pending_review to disputed (same as REST)
        queries::update_settlement_status(
            &state.db,
            id,
            "pending_review",
            "disputed",
            Some(&note),
            None,
            actor,
        )
        .await
        .map_err(|e| {
            if matches!(e, sqlx::Error::RowNotFound) {
                AppError::StaleTransition
            } else {
                AppError::DatabaseError(e.to_string())
            }
        })
        .into_gql()
    }

    /// Resolve a dispute by adjusting or voiding the settlement.
    ///
    /// # Arguments
    /// * `id` - The settlement UUID
    /// * `action` - "adjusted" (transitions to adjusted) or "voided" (transitions to voided)
    /// * `new_total` - New total amount when action is "adjusted"
    ///
    /// # Returns
    /// The updated settlement object
    async fn resolve_settlement_dispute(
        &self,
        ctx: &Context<'_>,
        id: Uuid,
        action: String,
        new_total: Option<String>,
    ) -> Result<Settlement> {
        let state = ctx.data::<AppState>()?;
        let actor = "gql-admin";

        // Parse new_total if provided
        let new_total_decimal = if let Some(amt) = new_total {
            Some(
                amt.parse::<BigDecimal>()
                    .map_err(|_| AppError::BadRequest("invalid amount format".to_string()))
                    .into_gql()?,
            )
        } else {
            None
        };

        // Validate action and update status
        let status = match action.as_str() {
            "adjusted" => "adjusted",
            "voided" => "voided",
            _ => {
                return Err(AppError::BadRequest(
                    "action must be 'adjusted' or 'voided'".to_string(),
                )
                .into_gql())
            }
        };

        // Call shared update_settlement_status (same as REST handler)
        queries::update_settlement_status(
            &state.db,
            id,
            "disputed",
            status,
            None,
            new_total_decimal.as_ref(),
            actor,
        )
        .await
        .map_err(|e| {
            if matches!(e, sqlx::Error::RowNotFound) {
                AppError::StaleTransition
            } else {
                AppError::DatabaseError(e.to_string())
            }
        })
        .into_gql()
    }
}
