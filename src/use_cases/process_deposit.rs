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
        crate::validation::validate_stellar_address(&input.stellar_account)
            .map_err(|e| RepositoryError::Validation(e.to_string()))?;
        crate::validation::validate_asset_code(&input.asset_code)
            .map_err(|e| RepositoryError::Validation(e.to_string()))?;
        crate::validation::validate_positive_amount(&input.amount)
            .map_err(|e| RepositoryError::Validation(e.to_string()))?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::RepositoryResult;
    use async_trait::async_trait;
    use std::str::FromStr;
    use std::sync::Mutex;

    struct InMemoryTransactionRepository {
        inserted: Mutex<Vec<Transaction>>,
    }

    impl InMemoryTransactionRepository {
        fn new() -> Self {
            Self {
                inserted: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl TransactionRepository for InMemoryTransactionRepository {
        async fn insert(&self, tx: &Transaction) -> RepositoryResult<Transaction> {
            self.inserted.lock().unwrap().push(tx.clone());
            Ok(tx.clone())
        }

        async fn get_by_id(&self, id: uuid::Uuid) -> RepositoryResult<Transaction> {
            self.inserted
                .lock()
                .unwrap()
                .iter()
                .find(|t| t.id == id)
                .cloned()
                .ok_or_else(|| RepositoryError::NotFound(id.to_string()))
        }

        async fn list(&self, _limit: i64, _offset: i64) -> RepositoryResult<Vec<Transaction>> {
            Ok(self.inserted.lock().unwrap().clone())
        }
    }

    fn valid_input() -> DepositInput {
        DepositInput {
            stellar_account: "G".to_owned() + &"A".repeat(55),
            amount: BigDecimal::from_str("10.50").unwrap(),
            asset_code: "USD".to_string(),
            anchor_transaction_id: None,
            callback_type: None,
            callback_status: None,
            memo: None,
            memo_type: None,
            metadata: None,
        }
    }

    #[tokio::test]
    async fn execute_accepts_valid_input() {
        let use_case = ProcessDeposit::new(Arc::new(InMemoryTransactionRepository::new()));
        assert!(use_case.execute(valid_input()).await.is_ok());
    }

    #[tokio::test]
    async fn execute_rejects_non_positive_amount() {
        let use_case = ProcessDeposit::new(Arc::new(InMemoryTransactionRepository::new()));
        let mut input = valid_input();
        input.amount = BigDecimal::from(0);
        assert!(matches!(
            use_case.execute(input).await,
            Err(RepositoryError::Validation(_))
        ));
    }

    #[tokio::test]
    async fn execute_rejects_negative_amount() {
        let use_case = ProcessDeposit::new(Arc::new(InMemoryTransactionRepository::new()));
        let mut input = valid_input();
        input.amount = BigDecimal::from(-5);
        assert!(matches!(
            use_case.execute(input).await,
            Err(RepositoryError::Validation(_))
        ));
    }

    #[tokio::test]
    async fn execute_rejects_invalid_stellar_account() {
        let use_case = ProcessDeposit::new(Arc::new(InMemoryTransactionRepository::new()));
        let mut input = valid_input();
        input.stellar_account = "BAD".to_string();
        assert!(matches!(
            use_case.execute(input).await,
            Err(RepositoryError::Validation(_))
        ));
    }

    #[tokio::test]
    async fn execute_rejects_invalid_asset_code() {
        let use_case = ProcessDeposit::new(Arc::new(InMemoryTransactionRepository::new()));
        let mut input = valid_input();
        input.asset_code = "usd".to_string();
        assert!(matches!(
            use_case.execute(input).await,
            Err(RepositoryError::Validation(_))
        ));
    }
}
