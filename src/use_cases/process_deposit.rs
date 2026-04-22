//! Process deposit use case.
//! Handles deposit logic using the TransactionRepository.

use crate::domain::Transaction;
use crate::ports::{RepositoryError, TransactionRepository};
use bigdecimal::BigDecimal;
use std::sync::Arc;

/// Input for the ProcessDeposit use case.
#[derive(Debug)]
pub struct DepositInput {
    pub stellar_account: String,
    pub amount: BigDecimal,
    pub asset_code: String,
    pub anchor_transaction_id: Option<String>,
    pub callback_type: Option<String>,
    pub callback_status: Option<String>,
    pub memo: Option<String>,
    pub memo_type: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

/// Output of the ProcessDeposit use case.
#[derive(Debug)]
pub struct DepositOutput {
    pub transaction_id: uuid::Uuid,
    pub success: bool,
}

/// Use case for processing deposits.
pub struct ProcessDeposit {
    transaction_repository: Arc<dyn TransactionRepository>,
}

impl ProcessDeposit {
    pub fn new(transaction_repository: Arc<dyn TransactionRepository>) -> Self {
        Self {
            transaction_repository,
        }
    }

    pub async fn execute(&self, input: DepositInput) -> Result<DepositOutput, RepositoryError> {
        let tx = Transaction::new(
            input.stellar_account,
            input.amount,
            input.asset_code,
            input.anchor_transaction_id,
            input.callback_type,
            input.callback_status,
            input.memo,
            input.memo_type,
            input.metadata,
        );

        let inserted = self.transaction_repository.insert(&tx).await?;

        Ok(DepositOutput {
            transaction_id: inserted.id,
            success: true,
        })
    }
}
