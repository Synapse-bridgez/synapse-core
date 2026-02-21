//! Process deposit use case.
//! Handles deposit logic using the TransactionRepository.

use crate::domain::Transaction;
use crate::ports::{RepositoryError, TransactionRepository};
use bigdecimal::BigDecimal;
use std::sync::Arc;

/// Input for the ProcessDeposit use case.
#[derive(Debug)]
pub struct DepositInput {
    pub id: String,
    pub anchor_transaction_id: String,
    pub stellar_account: Option<String>,
    pub amount: Option<BigDecimal>,
    pub asset_code: Option<String>,
}

/// Output of the ProcessDeposit use case.
#[derive(Debug)]
pub struct DepositOutput {
    pub success: bool,
    pub message: String,
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
            input
                .stellar_account
                .unwrap_or_else(|| "unknown".to_string()),
            input.amount.unwrap_or_else(|| BigDecimal::from(0)),
            input.asset_code.unwrap_or_else(|| "USD".to_string()),
            Some(input.anchor_transaction_id),
            Some("deposit".to_string()),
            Some("pending".to_string()),
        );

        self.transaction_repository.insert(&tx).await?;

        Ok(DepositOutput {
            success: true,
            message: format!("Webhook {} processed successfully", input.id),
        })
    }
}
