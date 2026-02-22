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
}

impl Transaction {
    pub fn new(
        stellar_account: String,
        amount: BigDecimal,
        asset_code: String,
        anchor_transaction_id: Option<String>,
        callback_type: Option<String>,
        callback_status: Option<String>,
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn test_transaction_new() {
        let stellar_account = "GBBD47UZQ5CSKQPV456PYYH4FSYJHBWGQJUVNMCNWZ2NBEHKQPW3KXKJ".to_string();
        let amount = BigDecimal::from_str("100.50").unwrap();
        let asset_code = "USDC".to_string();
        let anchor_id = Some("anchor_123".to_string());
        let callback_type = Some("deposit".to_string());
        let callback_status = Some("pending".to_string());

        let transaction = Transaction::new(
            stellar_account.clone(),
            amount.clone(),
            asset_code.clone(),
            anchor_id.clone(),
            callback_type.clone(),
            callback_status.clone(),
        );

        assert_eq!(transaction.stellar_account, stellar_account);
        assert_eq!(transaction.amount, amount);
        assert_eq!(transaction.asset_code, asset_code);
        assert_eq!(transaction.status, "pending");
        assert_eq!(transaction.anchor_transaction_id, anchor_id);
        assert_eq!(transaction.callback_type, callback_type);
        assert_eq!(transaction.callback_status, callback_status);
        assert!(transaction.created_at <= Utc::now());
        assert!(transaction.updated_at <= Utc::now());
    }

    #[test]
    fn test_transaction_new_with_none_values() {
        let stellar_account = "GBBD47UZQ5CSKQPV456PYYH4FSYJHBWGQJUVNMCNWZ2NBEHKQPW3KXKJ".to_string();
        let amount = BigDecimal::from_str("50.25").unwrap();
        let asset_code = "XLM".to_string();

        let transaction = Transaction::new(
            stellar_account.clone(),
            amount.clone(),
            asset_code.clone(),
            None,
            None,
            None,
        );

        assert_eq!(transaction.stellar_account, stellar_account);
        assert_eq!(transaction.amount, amount);
        assert_eq!(transaction.asset_code, asset_code);
        assert_eq!(transaction.status, "pending");
        assert_eq!(transaction.anchor_transaction_id, None);
        assert_eq!(transaction.callback_type, None);
        assert_eq!(transaction.callback_status, None);
    }

    #[test]
    fn test_transaction_clone() {
        let transaction = Transaction::new(
            "GBBD47UZQ5CSKQPV456PYYH4FSYJHBWGQJUVNMCNWZ2NBEHKQPW3KXKJ".to_string(),
            BigDecimal::from_str("100.0").unwrap(),
            "USDC".to_string(),
            Some("anchor_123".to_string()),
            Some("deposit".to_string()),
            Some("pending".to_string()),
        );

        let cloned = transaction.clone();
        assert_eq!(transaction.id, cloned.id);
        assert_eq!(transaction.stellar_account, cloned.stellar_account);
        assert_eq!(transaction.amount, cloned.amount);
        assert_eq!(transaction.asset_code, cloned.asset_code);
    }
}