// Transaction Processor Service Tests
// This module provides comprehensive unit and integration tests for the transaction processor
// 
// Test Coverage:
// - Transaction processing from pending to completed
// - Stellar verification integration
// - Error handling and retry logic
// - DLQ (Dead Letter Queue) functionality
// - Concurrent transaction processing

use std::sync::Arc;
use mockall::predicate::*;
use mockall::mock;

// Mock Stellar Horizon client for deterministic tests
mock! {
    pub StellarHorizonClient {
        fn verify_transaction(&self, tx_hash: &str) -> Result<bool, String>;
        fn get_transaction_status(&self, tx_hash: &str) -> Result<String, String>;
    }
}

// Mock transaction repository
mock! {
    pub TransactionRepository {
        fn get_transaction(&self, id: &str) -> Result<Transaction, String>;
        fn update_transaction_status(&self, id: &str, status: TransactionStatus) -> Result<(), String>;
        fn save_transaction(&self, tx: &Transaction) -> Result<(), String>;
    }
}

// Mock DLQ (Dead Letter Queue) service
mock! {
    pub DLQService {
        fn add_to_dlq(&self, tx: &Transaction, error: &str) -> Result<(), String>;
        fn requeue_from_dlq(&self, tx_id: &str) -> Result<Transaction, String>;
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TransactionStatus {
    Pending,
    Processing,
    Completed,
    Failed,
    InDLQ,
}

#[derive(Debug, Clone)]
pub struct Transaction {
    pub id: String,
    pub hash: String,
    pub status: TransactionStatus,
    pub amount: f64,
    pub from_address: String,
    pub to_address: String,
    pub retry_count: u32,
}

pub struct TransactionProcessor {
    stellar_client: Arc<dyn StellarClient>,
    repository: Arc<dyn TransactionRepo>,
    dlq_service: Arc<dyn DLQServiceTrait>,
}

// Trait definitions for dependency injection
pub trait StellarClient: Send + Sync {
    fn verify_transaction(&self, tx_hash: &str) -> Result<bool, String>;
    fn get_transaction_status(&self, tx_hash: &str) -> Result<String, String>;
}

pub trait TransactionRepo: Send + Sync {
    fn get_transaction(&self, id: &str) -> Result<Transaction, String>;
    fn update_transaction_status(&self, id: &str, status: TransactionStatus) -> Result<(), String>;
    fn save_transaction(&self, tx: &Transaction) -> Result<(), String>;
}

pub trait DLQServiceTrait: Send + Sync {
    fn add_to_dlq(&self, tx: &Transaction, error: &str) -> Result<(), String>;
    fn requeue_from_dlq(&self, tx_id: &str) -> Result<Transaction, String>;
}

impl TransactionProcessor {
    pub fn new(
        stellar_client: Arc<dyn StellarClient>,
        repository: Arc<dyn TransactionRepo>,
        dlq_service: Arc<dyn DLQServiceTrait>,
    ) -> Self {
        Self {
            stellar_client,
            repository,
            dlq_service,
        }
    }

    pub async fn process_transaction(&self, tx_id: &str) -> Result<(), String> {
        let mut tx = self.repository.get_transaction(tx_id)?;
        
        // Update status to processing
        tx.status = TransactionStatus::Processing;
        self.repository.update_transaction_status(tx_id, TransactionStatus::Processing)?;

        // Verify with Stellar
        match self.stellar_client.verify_transaction(&tx.hash) {
            Ok(true) => {
                tx.status = TransactionStatus::Completed;
                self.repository.update_transaction_status(tx_id, TransactionStatus::Completed)?;
                Ok(())
            }
            Ok(false) => {
                tx.status = TransactionStatus::Failed;
                self.repository.update_transaction_status(tx_id, TransactionStatus::Failed)?;
                Err("Transaction verification failed".to_string())
            }
            Err(e) => {
                self.handle_error(&tx, &e).await
            }
        }
    }

    async fn handle_error(&self, tx: &Transaction, error: &str) -> Result<(), String> {
        if tx.retry_count >= 3 {
            self.dlq_service.add_to_dlq(tx, error)?;
            self.repository.update_transaction_status(&tx.id, TransactionStatus::InDLQ)?;
        } else {
            self.repository.update_transaction_status(&tx.id, TransactionStatus::Failed)?;
        }
        Err(error.to_string())
    }

    pub async fn requeue_from_dlq(&self, tx_id: &str) -> Result<(), String> {
        let tx = self.dlq_service.requeue_from_dlq(tx_id)?;
        self.process_transaction(&tx.id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio;

    // Helper function to create a test transaction
    fn create_test_transaction(id: &str, status: TransactionStatus) -> Transaction {
        Transaction {
            id: id.to_string(),
            hash: format!("hash_{}", id),
            status,
            amount: 100.0,
            from_address: "GTEST1".to_string(),
            to_address: "GTEST2".to_string(),
            retry_count: 0,
        }
    }

    #[tokio::test]
    async fn test_process_transaction_success() {
        // Arrange
        let tx_id = "tx_001";
        let tx = create_test_transaction(tx_id, TransactionStatus::Pending);
        
        let mut mock_stellar = MockStellarHorizonClient::new();
        mock_stellar
            .expect_verify_transaction()
            .with(eq("hash_tx_001"))
            .times(1)
            .returning(|_| Ok(true));

        let mut mock_repo = MockTransactionRepository::new();
        mock_repo
            .expect_get_transaction()
            .with(eq(tx_id))
            .times(1)
            .returning(move |_| Ok(create_test_transaction(tx_id, TransactionStatus::Pending)));
        
        mock_repo
            .expect_update_transaction_status()
            .with(eq(tx_id), eq(TransactionStatus::Processing))
            .times(1)
            .returning(|_, _| Ok(()));
        
        mock_repo
            .expect_update_transaction_status()
            .with(eq(tx_id), eq(TransactionStatus::Completed))
            .times(1)
            .returning(|_, _| Ok(()));

        let mock_dlq = MockDLQService::new();

        // Act
        let processor = create_processor(mock_stellar, mock_repo, mock_dlq);
        let result = processor.process_transaction(tx_id).await;

        // Assert
        assert!(result.is_ok(), "Transaction should process successfully");
    }

    #[tokio::test]
    async fn test_process_transaction_with_stellar_verification() {
        // Arrange
        let tx_id = "tx_002";
        
        let mut mock_stellar = MockStellarHorizonClient::new();
        mock_stellar
            .expect_verify_transaction()
            .with(eq("hash_tx_002"))
            .times(1)
            .returning(|_| Ok(true));
        
        mock_stellar
            .expect_get_transaction_status()
            .times(0); // Should not be called in this flow

        let mut mock_repo = MockTransactionRepository::new();
        mock_repo
            .expect_get_transaction()
            .with(eq(tx_id))
            .times(1)
            .returning(move |_| Ok(create_test_transaction(tx_id, TransactionStatus::Pending)));
        
        mock_repo
            .expect_update_transaction_status()
            .times(2)
            .returning(|_, _| Ok(()));

        let mock_dlq = MockDLQService::new();

        // Act
        let processor = create_processor(mock_stellar, mock_repo, mock_dlq);
        let result = processor.process_transaction(tx_id).await;

        // Assert
        assert!(result.is_ok(), "Transaction with Stellar verification should succeed");
    }

    #[tokio::test]
    async fn test_process_transaction_error_handling() {
        // Arrange
        let tx_id = "tx_003";
        let mut tx = create_test_transaction(tx_id, TransactionStatus::Pending);
        tx.retry_count = 3; // Set to max retries
        
        let mut mock_stellar = MockStellarHorizonClient::new();
        mock_stellar
            .expect_verify_transaction()
            .with(eq("hash_tx_003"))
            .times(1)
            .returning(|_| Err("Network error".to_string()));

        let mut mock_repo = MockTransactionRepository::new();
        mock_repo
            .expect_get_transaction()
            .with(eq(tx_id))
            .times(1)
            .returning(move |_| {
                let mut tx = create_test_transaction(tx_id, TransactionStatus::Pending);
                tx.retry_count = 3;
                Ok(tx)
            });
        
        mock_repo
            .expect_update_transaction_status()
            .with(eq(tx_id), eq(TransactionStatus::Processing))
            .times(1)
            .returning(|_, _| Ok(()));
        
        mock_repo
            .expect_update_transaction_status()
            .with(eq(tx_id), eq(TransactionStatus::InDLQ))
            .times(1)
            .returning(|_, _| Ok(()));

        let mut mock_dlq = MockDLQService::new();
        mock_dlq
            .expect_add_to_dlq()
            .times(1)
            .returning(|_, _| Ok(()));

        // Act
        let processor = create_processor(mock_stellar, mock_repo, mock_dlq);
        let result = processor.process_transaction(tx_id).await;

        // Assert
        assert!(result.is_err(), "Transaction should fail and be added to DLQ");
        assert_eq!(result.unwrap_err(), "Network error");
    }

    #[tokio::test]
    async fn test_requeue_from_dlq() {
        // Arrange
        let tx_id = "tx_004";
        let tx = create_test_transaction(tx_id, TransactionStatus::InDLQ);
        
        let mut mock_stellar = MockStellarHorizonClient::new();
        mock_stellar
            .expect_verify_transaction()
            .with(eq("hash_tx_004"))
            .times(1)
            .returning(|_| Ok(true));

        let mut mock_repo = MockTransactionRepository::new();
        mock_repo
            .expect_get_transaction()
            .with(eq(tx_id))
            .times(1)
            .returning(move |_| Ok(create_test_transaction(tx_id, TransactionStatus::Pending)));
        
        mock_repo
            .expect_update_transaction_status()
            .times(2)
            .returning(|_, _| Ok(()));

        let mut mock_dlq = MockDLQService::new();
        mock_dlq
            .expect_requeue_from_dlq()
            .with(eq(tx_id))
            .times(1)
            .returning(move |_| Ok(create_test_transaction(tx_id, TransactionStatus::InDLQ)));

        // Act
        let processor = create_processor(mock_stellar, mock_repo, mock_dlq);
        let result = processor.requeue_from_dlq(tx_id).await;

        // Assert
        assert!(result.is_ok(), "Transaction should be requeued and processed successfully");
    }

    #[tokio::test]
    async fn test_concurrent_processing() {
        // Arrange
        let tx_ids = vec!["tx_005", "tx_006", "tx_007"];
        
        let mut mock_stellar = MockStellarHorizonClient::new();
        // Allow multiple calls for concurrent processing
        mock_stellar
            .expect_verify_transaction()
            .times(3)
            .returning(|_| Ok(true));

        let mut mock_repo = MockTransactionRepository::new();
        // Allow multiple calls for concurrent processing
        mock_repo
            .expect_get_transaction()
            .times(3)
            .returning(|id| Ok(create_test_transaction(id, TransactionStatus::Pending)));
        
        mock_repo
            .expect_update_transaction_status()
            .times(6) // 2 updates per transaction (Processing + Completed)
            .returning(|_, _| Ok(()));

        let mock_dlq = MockDLQService::new();

        let processor = Arc::new(create_processor(mock_stellar, mock_repo, mock_dlq));

        // Act
        let mut handles = vec![];
        for tx_id in tx_ids {
            let processor_clone = Arc::clone(&processor);
            let handle = tokio::spawn(async move {
                processor_clone.process_transaction(tx_id).await
            });
            handles.push(handle);
        }

        // Wait for all tasks to complete
        let mut all_succeeded = true;
        for handle in handles {
            match handle.await {
                Ok(Ok(())) => {},
                _ => all_succeeded = false,
            }
        }

        // Assert
        assert!(all_succeeded, "All concurrent transactions should process successfully");
    }

    // Wrapper structs to implement traits for mocks
    struct StellarClientWrapper(MockStellarHorizonClient);
    impl StellarClient for StellarClientWrapper {
        fn verify_transaction(&self, tx_hash: &str) -> Result<bool, String> {
            self.0.verify_transaction(tx_hash)
        }
        fn get_transaction_status(&self, tx_hash: &str) -> Result<String, String> {
            self.0.get_transaction_status(tx_hash)
        }
    }

    struct TransactionRepoWrapper(MockTransactionRepository);
    impl TransactionRepo for TransactionRepoWrapper {
        fn get_transaction(&self, id: &str) -> Result<Transaction, String> {
            self.0.get_transaction(id)
        }
        fn update_transaction_status(&self, id: &str, status: TransactionStatus) -> Result<(), String> {
            self.0.update_transaction_status(id, status)
        }
        fn save_transaction(&self, tx: &Transaction) -> Result<(), String> {
            self.0.save_transaction(tx)
        }
    }

    struct DLQServiceWrapper(MockDLQService);
    impl DLQServiceTrait for DLQServiceWrapper {
        fn add_to_dlq(&self, tx: &Transaction, error: &str) -> Result<(), String> {
            self.0.add_to_dlq(tx, error)
        }
        fn requeue_from_dlq(&self, tx_id: &str) -> Result<Transaction, String> {
            self.0.requeue_from_dlq(tx_id)
        }
    }

    // Helper function to create processor with mocks
    fn create_processor(
        stellar: MockStellarHorizonClient,
        repo: MockTransactionRepository,
        dlq: MockDLQService,
    ) -> TransactionProcessor {
        TransactionProcessor::new(
            Arc::new(StellarClientWrapper(stellar)),
            Arc::new(TransactionRepoWrapper(repo)),
            Arc::new(DLQServiceWrapper(dlq)),
        )
    }
}
