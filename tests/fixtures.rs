//! Shared test fixture factory for the `Transaction` model.
//!
//! # Usage
//! ```rust
//! let tx = TransactionFixture::new()
//!     .with_status("completed")
//!     .with_amount("500.00")
//!     .build();
//!
//! // Pre-built scenarios
//! let deposit  = TransactionFixture::pending_deposit();
//! let withdraw = TransactionFixture::completed_withdrawal();
//! let failed   = TransactionFixture::failed_transaction();
//! ```

use bigdecimal::BigDecimal;
use chrono::Utc;
use std::str::FromStr;
use synapse_core::db::models::Transaction;
use uuid::Uuid;

/// Builder for `Transaction` test fixtures with sensible defaults.
pub struct TransactionFixture {
    id: Uuid,
    stellar_account: String,
    amount: BigDecimal,
    asset_code: String,
    status: String,
    anchor_transaction_id: Option<String>,
    callback_type: Option<String>,
    callback_status: Option<String>,
    settlement_id: Option<Uuid>,
    memo: Option<String>,
    memo_type: Option<String>,
    metadata: Option<serde_json::Value>,
}

impl Default for TransactionFixture {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            stellar_account: "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAWHF"
                .to_string(),
            amount: BigDecimal::from_str("100.00").unwrap(),
            asset_code: "USD".to_string(),
            status: "pending".to_string(),
            anchor_transaction_id: None,
            callback_type: None,
            callback_status: None,
            settlement_id: None,
            memo: None,
            memo_type: None,
            metadata: None,
        }
    }
}

impl TransactionFixture {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_id(mut self, id: Uuid) -> Self {
        self.id = id;
        self
    }

    pub fn with_stellar_account(mut self, account: &str) -> Self {
        self.stellar_account = account.to_string();
        self
    }

    pub fn with_amount(mut self, amount: &str) -> Self {
        self.amount = BigDecimal::from_str(amount).expect("invalid amount");
        self
    }

    pub fn with_asset_code(mut self, code: &str) -> Self {
        self.asset_code = code.to_string();
        self
    }

    pub fn with_status(mut self, status: &str) -> Self {
        self.status = status.to_string();
        self
    }

    pub fn with_anchor_transaction_id(mut self, id: &str) -> Self {
        self.anchor_transaction_id = Some(id.to_string());
        self
    }

    pub fn with_callback_type(mut self, t: &str) -> Self {
        self.callback_type = Some(t.to_string());
        self
    }

    pub fn with_callback_status(mut self, s: &str) -> Self {
        self.callback_status = Some(s.to_string());
        self
    }

    pub fn with_settlement_id(mut self, id: Uuid) -> Self {
        self.settlement_id = Some(id);
        self
    }

    pub fn with_memo(mut self, memo: &str, memo_type: &str) -> Self {
        self.memo = Some(memo.to_string());
        self.memo_type = Some(memo_type.to_string());
        self
    }

    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Consume the builder and produce a `Transaction`.
    pub fn build(self) -> Transaction {
        let now = Utc::now();
        Transaction {
            id: self.id,
            stellar_account: self.stellar_account,
            amount: self.amount,
            asset_code: self.asset_code,
            status: self.status,
            created_at: now,
            updated_at: now,
            anchor_transaction_id: self.anchor_transaction_id,
            callback_type: self.callback_type,
            callback_status: self.callback_status,
            settlement_id: self.settlement_id,
            memo: self.memo,
            memo_type: self.memo_type,
            metadata: self.metadata,
        }
    }

    // ── Pre-built scenarios ───────────────────────────────────────────────────

    /// A pending deposit of 100 USD.
    pub fn pending_deposit() -> Transaction {
        Self::new()
            .with_status("pending")
            .with_callback_type("deposit")
            .with_callback_status("pending_external")
            .build()
    }

    /// A completed withdrawal of 250 USD.
    pub fn completed_withdrawal() -> Transaction {
        Self::new()
            .with_amount("250.00")
            .with_status("completed")
            .with_callback_type("withdrawal")
            .with_callback_status("completed")
            .build()
    }

    /// A failed transaction with error metadata.
    pub fn failed_transaction() -> Transaction {
        Self::new()
            .with_status("error")
            .with_callback_status("error")
            .with_metadata(serde_json::json!({ "error_code": "INSUFFICIENT_FUNDS" }))
            .build()
    }
}

// ── Unit tests for the fixture factory itself ─────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_fixture_has_pending_status() {
        let tx = TransactionFixture::new().build();
        assert_eq!(tx.status, "pending");
        assert_eq!(tx.asset_code, "USD");
    }

    #[test]
    fn test_builder_overrides_status() {
        let tx = TransactionFixture::new().with_status("completed").build();
        assert_eq!(tx.status, "completed");
    }

    #[test]
    fn test_builder_overrides_amount() {
        let tx = TransactionFixture::new().with_amount("500.00").build();
        assert_eq!(tx.amount.to_string(), "500.00");
    }

    #[test]
    fn test_pending_deposit_scenario() {
        let tx = TransactionFixture::pending_deposit();
        assert_eq!(tx.status, "pending");
        assert_eq!(tx.callback_type.as_deref(), Some("deposit"));
    }

    #[test]
    fn test_completed_withdrawal_scenario() {
        let tx = TransactionFixture::completed_withdrawal();
        assert_eq!(tx.status, "completed");
        assert_eq!(tx.callback_type.as_deref(), Some("withdrawal"));
    }

    #[test]
    fn test_failed_transaction_scenario() {
        let tx = TransactionFixture::failed_transaction();
        assert_eq!(tx.status, "error");
        assert!(tx.metadata.is_some());
    }

    #[test]
    fn test_memo_fields() {
        let tx = TransactionFixture::new()
            .with_memo("invoice-42", "text")
            .build();
        assert_eq!(tx.memo.as_deref(), Some("invoice-42"));
        assert_eq!(tx.memo_type.as_deref(), Some("text"));
    }

    #[test]
    fn test_unique_ids_per_build() {
        let tx1 = TransactionFixture::new().build();
        let tx2 = TransactionFixture::new().build();
        assert_ne!(tx1.id, tx2.id);
    }
}
