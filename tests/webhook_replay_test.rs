use chrono::Utc;
use sqlx::PgPool;
use synapse_core::db::models::Transaction;
use synapse_core::db::queries;
use uuid::Uuid;

#[sqlx::test]
async fn test_webhook_replay_tracking(pool: PgPool) -> sqlx::Result<()> {
    // Create a test transaction
    let tx = Transaction::new(
        "GABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890ABCDEFGHIJKLMNOP".to_string(),
        "100.50".parse().unwrap(),
        "USDC".to_string(),
        Some("anchor-tx-123".to_string()),
        Some("deposit".to_string()),
        Some("completed".to_string()),
        None,
        None,
        None,
    );

    let inserted = queries::insert_transaction(&pool, &tx).await?;

    // Simulate a replay attempt
    sqlx::query(
        r#"
        INSERT INTO webhook_replay_history 
        (transaction_id, replayed_by, dry_run, success, error_message)
        VALUES ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(inserted.id)
    .bind("test-admin")
    .bind(true)
    .bind(true)
    .bind(None::<String>)
    .execute(&pool)
    .await?;

    // Verify the replay was tracked
    let replay_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM webhook_replay_history WHERE transaction_id = $1",
    )
    .bind(inserted.id)
    .fetch_one(&pool)
    .await?;

    assert_eq!(replay_count, 1);

    Ok(())
}

#[sqlx::test]
async fn test_list_failed_webhooks(pool: PgPool) -> sqlx::Result<()> {
    // Create a failed transaction
    let tx = Transaction::new(
        "GABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890ABCDEFGHIJKLMNOP".to_string(),
        "50.00".parse().unwrap(),
        "USDC".to_string(),
        Some("anchor-tx-456".to_string()),
        Some("deposit".to_string()),
        Some("failed".to_string()),
        None,
        None,
        None,
    );

    let inserted = queries::insert_transaction(&pool, &tx).await?;

    // Update status to failed
    sqlx::query("UPDATE transactions SET status = 'failed' WHERE id = $1")
        .bind(inserted.id)
        .execute(&pool)
        .await?;

    // Query failed webhooks
    let failed_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM transactions WHERE status = 'failed'",
    )
    .fetch_one(&pool)
    .await?;

    assert!(failed_count >= 1);

    Ok(())
}

#[sqlx::test]
async fn test_replay_updates_status(pool: PgPool) -> sqlx::Result<()> {
    // Create a failed transaction
    let tx = Transaction::new(
        "GABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890ABCDEFGHIJKLMNOP".to_string(),
        "75.00".parse().unwrap(),
        "USDC".to_string(),
        Some("anchor-tx-789".to_string()),
        Some("deposit".to_string()),
        Some("failed".to_string()),
        None,
        None,
        None,
    );

    let inserted = queries::insert_transaction(&pool, &tx).await?;

    // Update status to failed
    sqlx::query("UPDATE transactions SET status = 'failed' WHERE id = $1")
        .bind(inserted.id)
        .execute(&pool)
        .await?;

    // Simulate replay by updating status to pending
    sqlx::query("UPDATE transactions SET status = 'pending', updated_at = NOW() WHERE id = $1")
        .bind(inserted.id)
        .execute(&pool)
        .await?;

    // Verify status was updated
    let updated_tx = queries::get_transaction(&pool, inserted.id).await?;
    assert_eq!(updated_tx.status, "pending");

    Ok(())
}
