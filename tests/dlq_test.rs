use synapse_core::services::TransactionProcessor;
use synapse_core::db::models::{Transaction, TransactionDlq};
use sqlx::PgPool;
use bigdecimal::BigDecimal;
use std::str::FromStr;
use sqlx::migrate::Migrator;
use std::path::Path;

async fn setup_db(pool: &PgPool) {
    let migrator =
        Migrator::new(Path::join(Path::new(env!("CARGO_MANIFEST_DIR")), "migrations")).await;
    if let Ok(m) = migrator {
        let _ = m.run(pool).await;
    }
}

#[tokio::test]
async fn test_dlq_workflow() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(v) => v,
        Err(_) => {
            println!("Skipping DLQ test: DATABASE_URL not set");
            return;
        }
    };
    
    let pool = PgPool::connect(&database_url).await.expect("Failed to connect to test DB");
    setup_db(&pool).await;
    
    // Create a test transaction
    let tx_id = uuid::Uuid::new_v4();
    let amount = BigDecimal::from_str("100.50").unwrap();
    
    sqlx::query(
        r#"
        INSERT INTO transactions (
            id, stellar_account, amount, asset_code, status
        ) VALUES ($1, $2, $3, $4, $5)
        "#
    )
    .bind(tx_id)
    .bind("GABCD1234TEST")
    .bind(&amount)
    .bind("USD")
    .bind("pending")
    .execute(&pool)
    .await
    .expect("Failed to insert test transaction");
    
    // Process transaction (will succeed in this simple case)
    let processor = TransactionProcessor::new(pool.clone());
    let result = processor.process_transaction(tx_id).await;
    
    assert!(result.is_ok(), "Transaction processing should succeed");
    
    // Verify transaction status updated
    let tx = sqlx::query_as::<_, Transaction>("SELECT * FROM transactions WHERE id = $1")
        .bind(tx_id)
        .fetch_one(&pool)
        .await
        .expect("Failed to fetch transaction");
    
    assert_eq!(tx.status, "completed");
    
    println!("✓ DLQ workflow test passed");
}

#[tokio::test]
async fn test_requeue_dlq() {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(v) => v,
        Err(_) => {
            println!("Skipping DLQ test: DATABASE_URL not set");
            return;
        }
    };
    
    let pool = PgPool::connect(&database_url).await.expect("Failed to connect to test DB");
    setup_db(&pool).await;
    
    // Create a test transaction
    let tx_id = uuid::Uuid::new_v4();
    let amount = BigDecimal::from_str("100.50").unwrap();
    
    sqlx::query(
        r#"
        INSERT INTO transactions (
            id, stellar_account, amount, asset_code, status
        ) VALUES ($1, $2, $3, $4, $5)
        "#
    )
    .bind(tx_id)
    .bind("GABCD1234TEST")
    .bind(&amount)
    .bind("USD")
    .bind("dlq")
    .execute(&pool)
    .await
    .expect("Failed to insert test transaction");
    
    // Create a DLQ entry
    let dlq_id = uuid::Uuid::new_v4();
    sqlx::query(
        r#"
        INSERT INTO transaction_dlq (
            id, transaction_id, stellar_account, amount, asset_code,
            error_reason, retry_count, original_created_at
        ) VALUES ($1, $2, $3, $4, $5, $6, $7, NOW())
        "#
    )
    .bind(dlq_id)
    .bind(tx_id)
    .bind("GABCD1234TEST")
    .bind(&amount)
    .bind("USD")
    .bind("Test error")
    .bind(3)
    .execute(&pool)
    .await
    .expect("Failed to insert DLQ entry");
    
    // Requeue the DLQ entry
    let processor = TransactionProcessor::new(pool.clone());
    let result = processor.requeue_dlq(dlq_id).await;
    
    assert!(result.is_ok(), "Requeue should succeed");
    
    // Verify transaction status reset to pending
    let tx = sqlx::query_as::<_, Transaction>("SELECT * FROM transactions WHERE id = $1")
        .bind(tx_id)
        .fetch_one(&pool)
        .await
        .expect("Failed to fetch transaction");
    
    assert_eq!(tx.status, "pending");
    
    // Verify DLQ entry removed
    let dlq_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM transaction_dlq WHERE id = $1")
        .bind(dlq_id)
        .fetch_one(&pool)
        .await
        .expect("Failed to count DLQ entries");
    
    assert_eq!(dlq_count, 0, "DLQ entry should be removed");
    
    println!("✓ Requeue DLQ test passed");
}
