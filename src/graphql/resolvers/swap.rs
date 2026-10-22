use crate::graphql::error::GqlResultExt;
use crate::services::swap_orders::{self, ExposureRow, SwapOrderRow};
use crate::AppState;
use async_graphql::{ComplexObject, Context, Object, Result, SimpleObject};
use chrono::{DateTime, Utc};
use sqlx::types::BigDecimal;
use std::str::FromStr;
use uuid::Uuid;

#[derive(SimpleObject, Clone)]
pub struct Quote {
    pub rate: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(SimpleObject, Clone)]
pub struct Execution {
    pub amount: Option<String>,
    pub rate: Option<String>,
    pub executed_at: DateTime<Utc>,
}

#[derive(SimpleObject, Clone)]
pub struct Settlement {
    pub settlement_id: Option<Uuid>,
    pub settled_at: DateTime<Utc>,
}

#[derive(SimpleObject, Clone)]
pub struct RiskExposure {
    pub asset: String,
    pub open_orders: i64,
    pub total_amount: String,
}

#[derive(SimpleObject, Clone)]
#[graphql(complex)]
pub struct SwapOrder {
    pub id: Uuid,
    pub account: String,
    pub source_asset: String,
    pub target_asset: String,
    pub amount: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[graphql(skip)]
    row: SwapOrderRow,
}

// Quote/execution/settlement are derived from the already-fetched row, so
// nesting them costs no extra queries (no N+1).
#[ComplexObject]
impl SwapOrder {
    async fn quote(&self) -> Quote {
        Quote {
            rate: self.row.quote_rate.to_string(),
            expires_at: self.row.quote_expires_at,
        }
    }

    async fn execution(&self) -> Option<Execution> {
        self.row.executed_at.map(|executed_at| Execution {
            amount: self.row.executed_amount.as_ref().map(|a| a.to_string()),
            rate: self.row.executed_rate.as_ref().map(|r| r.to_string()),
            executed_at,
        })
    }

    async fn settlement(&self) -> Option<Settlement> {
        self.row.settled_at.map(|settled_at| Settlement {
            settlement_id: self.row.settlement_id,
            settled_at,
        })
    }
}

impl From<SwapOrderRow> for SwapOrder {
    fn from(row: SwapOrderRow) -> Self {
        Self {
            id: row.id,
            account: row.account.clone(),
            source_asset: row.source_asset.clone(),
            target_asset: row.target_asset.clone(),
            amount: row.amount.to_string(),
            status: row.status.clone(),
            created_at: row.created_at,
            updated_at: row.updated_at,
            row,
        }
    }
}

impl From<ExposureRow> for RiskExposure {
    fn from(row: ExposureRow) -> Self {
        Self {
            asset: row.asset,
            open_orders: row.open_orders,
            total_amount: row.total_amount.to_string(),
        }
    }
}

fn parse_decimal(field: &str, value: &str) -> Result<BigDecimal> {
    BigDecimal::from_str(value)
        .map_err(|_| async_graphql::Error::new(format!("{field} must be a decimal number")))
}

#[derive(Default)]
pub struct SwapQuery;

#[Object]
impl SwapQuery {
    async fn swap_order(&self, ctx: &Context<'_>, id: Uuid) -> Result<SwapOrder> {
        let state = ctx.data::<AppState>()?;
        swap_orders::get_order(&state.db, id)
            .await
            .map(Into::into)
            .into_gql()
    }

    /// Page size is capped at 100 by the service layer.
    #[graphql(complexity = "limit.unwrap_or(20).clamp(1, 100) as usize * child_complexity")]
    async fn swap_orders_by_account(
        &self,
        ctx: &Context<'_>,
        account: String,
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> Result<Vec<SwapOrder>> {
        let state = ctx.data::<AppState>()?;
        swap_orders::list_orders_by_account(&state.db, &account, limit, offset)
            .await
            .map(|rows| rows.into_iter().map(Into::into).collect())
            .into_gql()
    }

    async fn risk_exposure(&self, ctx: &Context<'_>) -> Result<Vec<RiskExposure>> {
        let state = ctx.data::<AppState>()?;
        swap_orders::current_exposure(&state.db)
            .await
            .map(|rows| rows.into_iter().map(Into::into).collect())
            .into_gql()
    }
}

#[derive(Default)]
pub struct SwapMutation;

#[Object]
impl SwapMutation {
    async fn create_swap_order(
        &self,
        ctx: &Context<'_>,
        account: String,
        source_asset: String,
        target_asset: String,
        amount: String,
        quote_rate: String,
    ) -> Result<SwapOrder> {
        let state = ctx.data::<AppState>()?;
        let amount = parse_decimal("amount", &amount)?;
        let quote_rate = parse_decimal("quoteRate", &quote_rate)?;
        swap_orders::create_order(
            &state.db,
            &account,
            &source_asset,
            &target_asset,
            amount,
            quote_rate,
        )
        .await
        .map(Into::into)
        .into_gql()
    }

    async fn cancel_swap_order(&self, ctx: &Context<'_>, id: Uuid) -> Result<SwapOrder> {
        let state = ctx.data::<AppState>()?;
        swap_orders::cancel_order(&state.db, id)
            .await
            .map(Into::into)
            .into_gql()
    }
}
