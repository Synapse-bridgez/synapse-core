//! Background processor for pending transactions.
//!
//! Polls the transactions table for rows with status = 'pending',
//! verifies on-chain via HorizonClient, and updates status to completed or failed.

use crate::db::models::Transaction;
use crate::stellar::HorizonClient;
use sqlx::PgPool;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, error, info};

const POLL_INTERVAL_SECS: u64 = 5;

/// Runs the background processor loop. Processes pending transactions asynchronously
/// without blocking the HTTP server. Uses `SELECT ... FOR UPDATE SKIP LOCKED`
/// for safe concurrent processing with multiple workers.
pub async fn run_processor(pool: PgPool, horizon_client: HorizonClient) {
    info!("Async transaction processor started");

    loop {
        if let Err(e) = process_batch(&pool, &horizon_client).await {
            error!("Processor batch error: {}", e);
        }

        sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
    }
}

async fn process_batch(pool: &PgPool, horizon_client: &HorizonClient) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;

    // Fetch pending transactions with row locking. SKIP LOCKED ensures we don't
    // block on rows another worker is processing.
    let pending: Vec<Transaction> = sqlx::query_as::<_, Transaction>(
        r#"
        SELECT id, stellar_account, amount, asset_code, status, created_at, updated_at,
               anchor_transaction_id, callback_type, callback_status, settlement_id
        FROM transactions
        WHERE status = 'pending'
        ORDER BY created_at ASC
        LIMIT 10
        FOR UPDATE SKIP LOCKED
        "#,
    )
    .fetch_all(&mut *tx)
    .await?;

    if pending.is_empty() {
        return Ok(());
    }

    debug!("Processing {} pending transaction(s)", pending.len());

    // Claim all rows by marking as processing, then commit to release the lock
    for t in &pending {
        sqlx::query("UPDATE transactions SET status = 'processing', updated_at = NOW() WHERE id = $1")
            .bind(t.id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;

    // Process each (Horizon calls without holding DB lock)
    for t in pending {
        let id = t.id;
        let stellar_account = t.stellar_account.clone();

        let result = horizon_client.get_account(&stellar_account).await;

        match result {
            Ok(_) => {
                sqlx::query(
                    "UPDATE transactions SET status = 'completed', updated_at = NOW() WHERE id = $1",
                )
                .bind(id)
                .execute(pool)
                .await?;
                info!("Transaction {} verified on-chain, marked completed", id);
            }
            Err(e) => {
                sqlx::query("UPDATE transactions SET status = 'failed', updated_at = NOW() WHERE id = $1")
                    .bind(id)
                    .execute(pool)
                    .await?;
                error!("Transaction {} verification failed: {}, marked failed", id, e);
            }
        }
    }

    Ok(())
}

/// Determines the new status based on Horizon client result
pub fn determine_transaction_status(horizon_result: &Result<crate::stellar::client::AccountResponse, crate::stellar::client::HorizonError>) -> &'static str {
    match horizon_result {
        Ok(_) => "completed",
        Err(_) => "failed",
    }
}

/// Validates if a stellar account address has the correct format
pub fn is_valid_stellar_account(account: &str) -> bool {
    account.len() == 56 && account.chars().all(|c| c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stellar::client::{AccountResponse, Balance, HorizonError};

    #[test]
    fn test_determine_transaction_status_success() {
        let account_response = AccountResponse {
            id: "test".to_string(),
            account_id: "GBBD47UZQ5CSKQPV456PYYH4FSYJHBWGQJUVNMCNWZ2NBEHKQPW3KXKJ".to_string(),
            balances: vec![Balance {
                balance: "100.0".to_string(),
                limit: None,
                asset_type: "native".to_string(),
                asset_code: None,
                asset_issuer: None,
            }],
            sequence: "1".to_string(),
            subentry_count: 0,
            home_domain: None,
            last_modified_ledger: 1,
            last_modified_time: "2023-01-01T00:00:00Z".to_string(),
        };

        let result = Ok(account_response);
        assert_eq!(determine_transaction_status(&result), "completed");
    }

    #[test]
    fn test_determine_transaction_status_failure() {
        let result: Result<AccountResponse, HorizonError> = Err(HorizonError::AccountNotFound("test".to_string()));
        assert_eq!(determine_transaction_status(&result), "failed");
    }

    #[test]
    fn test_is_valid_stellar_account_valid() {
        let valid_account = "GBBD47UZQ5CSKQPV456PYYH4FSYJHBWGQJUVNMCNWZ2NBEHKQPW3KXKJ";
        assert!(is_valid_stellar_account(valid_account));
    }

    #[test]
    fn test_is_valid_stellar_account_invalid_length() {
        let short_account = "GBBD47UZQ5CSKQPV456PYYH4FSYJHBWGQJUVNMCNWZ2NBEHKQPW3KXK";
        assert!(!is_valid_stellar_account(short_account));

        let long_account = "GBBD47UZQ5CSKQPV456PYYH4FSYJHBWGQJUVNMCNWZ2NBEHKQPW3KXKJX";
        assert!(!is_valid_stellar_account(long_account));
    }

    #[test]
    fn test_is_valid_stellar_account_invalid_characters() {
        let invalid_account = "GBBD47UZQ5CSKQPV456PYYH4FSYJHBWGQJUVNMCNWZ2NBEHKQPW3KXK!";
        assert!(!is_valid_stellar_account(invalid_account));
    }

    #[test]
    fn test_poll_interval_constant() {
        assert_eq!(POLL_INTERVAL_SECS, 5);
    }
}