use serde::{Deserialize, Serialize};
use uuid::Uuid;
use chrono::{DateTime, Utc};

/// Transaction schema for OpenAPI documentation
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct TransactionSchema {
    /// Unique transaction identifier
    pub id: String,
    /// Stellar account address
    pub stellar_account: String,
    /// Transaction amount as string to preserve precision
    pub amount: String,
    /// Asset code (e.g., USD)
    pub asset_code: String,
    /// Current transaction status
    pub status: String,
    /// Timestamp when transaction was created
    pub created_at: DateTime<Utc>,
    /// Timestamp when transaction was last updated
    pub updated_at: DateTime<Utc>,
    /// Associated anchor transaction ID
    pub anchor_transaction_id: Option<String>,
    /// Type of callback
    pub callback_type: Option<String>,
    /// Status from callback
    pub callback_status: Option<String>,
    /// Associated settlement ID
    pub settlement_id: Option<String>,
    pub memo: Option<String>,
    pub memo_type: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

/// Settlement schema for OpenAPI documentation
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct SettlementSchema {
    /// Unique settlement identifier
    pub id: String,
    /// Asset code for settlement
    pub asset_code: String,
    /// Total settlement amount as string
    pub total_amount: String,
    /// Number of transactions in settlement
    pub tx_count: i32,
    /// Settlement period start
    pub period_start: DateTime<Utc>,
    /// Settlement period end
    pub period_end: DateTime<Utc>,
    /// Current settlement status
    pub status: String,
    pub updated_at: DateTime<Utc>,
}