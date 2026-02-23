use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Tenant {
    pub tenant_id: Uuid,
    pub name: String,
    pub api_key: String,
    pub webhook_secret: String,
    pub stellar_account: String,
    pub rate_limit_per_minute: i32,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Transaction {
    pub transaction_id: Uuid,
    pub tenant_id: Uuid,
    pub external_id: String,
    pub status: String,
    pub amount: String,
    pub asset_code: String,
    pub stellar_transaction_id: Option<String>,
    pub memo: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateTransactionRequest {
    pub external_id: String,
    pub amount: String,
    pub asset_code: String,
    pub memo: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTransactionRequest {
    pub status: Option<String>,
    pub stellar_transaction_id: Option<String>,
}
