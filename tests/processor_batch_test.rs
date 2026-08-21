use sqlx::PgPool;
use synapse_core::services::processor::process_batch;
use synapse_core::stellar::HorizonClient;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires database"]
async fn test_process_batch_calls_horizon_verification() {
    let pool = setup_test_db().await;
    let horizon_client = setup_horizon_client();

    let transaction_id = Uuid::new_v4();
    let stellar_account = "GBYLXUZRYT4NTC4WQH6QLAEAHA4ZXFHIOJCDC7M4TI6JBLTHSNJ4LYPG";

    insert_pending_transaction(&pool, transaction_id, stellar_account.to_string()).await;

    let processed = process_batch(&pool, &horizon_client, 10)
        .await
        .expect("process_batch failed");

    assert_eq!(processed, 1, "Should process exactly one transaction");

    let tx_in_db: (String,) = sqlx::query_as("SELECT status FROM transactions WHERE id = $1")
        .bind(transaction_id)
        .fetch_one(&pool)
        .await
        .expect("Failed to fetch transaction");

    assert_ne!(
        tx_in_db.0, "pending",
        "Transaction status should change from pending (either completed or failed)"
    );
}

#[tokio::test]
#[ignore = "requires database"]
async fn test_process_batch_updates_transaction_status() {
    let pool = setup_test_db().await;
    let horizon_client = setup_horizon_client();

    let transaction_id = Uuid::new_v4();
    let stellar_account = "GBYLXUZRYT4NTC4WQH6QLAEAHA4ZXFHIOJCDC7M4TI6JBLTHSNJ4LYPG";

    insert_pending_transaction(&pool, transaction_id, stellar_account.to_string()).await;

    let _processed = process_batch(&pool, &horizon_client, 10)
        .await
        .expect("process_batch failed");

    let updated_at: (i64,) = sqlx::query_as(
        "SELECT EXTRACT(EPOCH FROM updated_at)::bigint FROM transactions WHERE id = $1",
    )
    .bind(transaction_id)
    .fetch_one(&pool)
    .await
    .expect("Failed to fetch transaction updated_at");

    let now_epoch = chrono::Utc::now().timestamp();
    assert!(
        (now_epoch - updated_at.0).abs() < 5,
        "updated_at should be recently set"
    );
}

#[tokio::test]
#[ignore = "requires database"]
async fn test_process_batch_empty_pending_returns_zero() {
    let pool = setup_test_db().await;
    let horizon_client = setup_horizon_client();

    let processed = process_batch(&pool, &horizon_client, 10)
        .await
        .expect("process_batch failed");

    assert_eq!(
        processed, 0,
        "Should return 0 when no pending transactions exist"
    );
}

#[tokio::test]
#[ignore = "requires database"]
async fn test_process_batch_respects_batch_size() {
    let pool = setup_test_db().await;
    let horizon_client = setup_horizon_client();

    let stellar_account = "GBYLXUZRYT4NTC4WQH6QLAEAHA4ZXFHIOJCDC7M4TI6JBLTHSNJ4LYPG";

    for _ in 0..15 {
        let transaction_id = Uuid::new_v4();
        insert_pending_transaction(&pool, transaction_id, stellar_account.to_string()).await;
    }

    let processed = process_batch(&pool, &horizon_client, 10)
        .await
        .expect("process_batch failed");

    assert_eq!(
        processed, 10,
        "Should process up to batch_size transactions"
    );

    let remaining: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM transactions WHERE status = 'pending'")
            .fetch_one(&pool)
            .await
            .expect("Failed to count pending");

    assert!(remaining > 0, "Some transactions should remain pending");
}

async fn setup_test_db() -> PgPool {
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://localhost/synapse_test".to_string());

    let pool = PgPool::connect(&database_url)
        .await
        .expect("Failed to connect to test database");

    pool
}

fn setup_horizon_client() -> HorizonClient {
    let horizon_url = std::env::var("STELLAR_HORIZON_URL")
        .unwrap_or_else(|_| "https://horizon-testnet.stellar.org".to_string());

    HorizonClient::new(horizon_url)
}

async fn insert_pending_transaction(pool: &PgPool, id: Uuid, stellar_account: String) {
    sqlx::query(
        "INSERT INTO transactions (id, stellar_account, amount, asset_code, status, created_at, updated_at, memo_type, callback_type)
         VALUES ($1, $2, $3::numeric, $4, $5, NOW(), NOW(), $6, $7)"
    )
    .bind(id)
    .bind(stellar_account)
    .bind("100.00")
    .bind("USDC")
    .bind("pending")
    .bind("text")
    .bind("idempotency")
    .execute(pool)
    .await
    .expect("Failed to insert test transaction");
}
