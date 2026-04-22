//! Transaction domain entity.
//! Framework-agnostic representation of a financial transaction.

use bigdecimal::BigDecimal;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Domain entity representing a transaction.
#[derive(Debug, Clone)]
pub struct Transaction {
    pub id: Uuid,
    pub stellar_account: String,
    pub amount: BigDecimal,
    pub asset_code: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub anchor_transaction_id: Option<String>,
    pub callback_type: Option<String>,
    pub callback_status: Option<String>,
    pub memo: Option<String>,
    pub memo_type: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

impl Transaction {
    pub fn new(
        stellar_account: String,
        amount: BigDecimal,
        asset_code: String,
        anchor_transaction_id: Option<String>,
        callback_type: Option<String>,
        callback_status: Option<String>,
        memo: Option<String>,
        memo_type: Option<String>,
        metadata: Option<serde_json::Value>,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4(),
            stellar_account,
            amount,
            asset_code,
            status: "pending".to_string(),
            created_at: now,
            updated_at: now,
            anchor_transaction_id,
            callback_type,
            callback_status,
            memo,
            memo_type,
            metadata,
        }
    }
}
