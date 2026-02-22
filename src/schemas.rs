use serde::{Deserialize, Serialize};
use bigdecimal::BigDecimal;
use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize)]
pub struct SettlementSchema {
    pub id: Uuid,
    pub asset_code: String,
    pub total_amount: BigDecimal,
    pub transaction_count: i64,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SettlementListResponse {
    pub settlements: Vec<SettlementSchema>,
    pub total: i64,
    pub page: i32,
    pub per_page: i32,
}