//! Lifecycle event log for swap order execution attempts (`swap_order_events`).

use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapEventType {
    Started,
    Submitted,
    PartiallyFilled,
    Filled,
    Failed,
}

impl SwapEventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            SwapEventType::Started => "started",
            SwapEventType::Submitted => "submitted",
            SwapEventType::PartiallyFilled => "partially_filled",
            SwapEventType::Filled => "filled",
            SwapEventType::Failed => "failed",
        }
    }
}

pub async fn record_event(
    pool: &PgPool,
    order_id: Uuid,
    attempt: i32,
    event: SwapEventType,
    detail: serde_json::Value,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO swap_order_events (order_id, attempt, event_type, detail) VALUES ($1, $2, $3, $4)",
    )
    .bind(order_id)
    .bind(attempt)
    .bind(event.as_str())
    .bind(detail)
    .execute(pool)
    .await?;
    Ok(())
}
