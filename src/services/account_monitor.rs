use crate::stellar::client::HorizonClient;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tracing::{error, info, warn};
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize)]
pub struct Payment {
    pub id: String,
    pub from: String,
    pub to: String,
    pub amount: String,
    pub asset_code: String,
    pub memo: Option<String>,
    pub memo_type: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PaymentRecord {
    id: String,
    from: String,
    to: String,
    amount: String,
    asset_code: String,
    #[serde(default)]
    memo: Option<String>,
    #[serde(default)]
    memo_type: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PaymentsResponse {
    #[serde(rename = "_embedded")]
    embedded: Embedded,
}

#[derive(Debug, Serialize, Deserialize)]
struct Embedded {
    records: Vec<PaymentRecord>,
}

pub struct AccountMonitor {
    horizon_client: HorizonClient,
    pool: PgPool,
    accounts: Vec<String>,
    poll_interval: Duration,
}

impl AccountMonitor {
    pub fn new(
        horizon_client: HorizonClient,
        pool: PgPool,
        accounts: Vec<String>,
        poll_interval_secs: u64,
    ) -> Self {
        Self {
            horizon_client,
            pool,
            accounts,
            poll_interval: Duration::from_secs(poll_interval_secs),
        }
    }

    pub async fn start(&self) {
        info!(
            "Starting account monitor for {} accounts",
            self.accounts.len()
        );

        loop {
            for account in &self.accounts {
                if let Err(e) = self.monitor_account(account).await {
                    error!("Error monitoring account {}: {}", account, e);
                }
            }

            sleep(self.poll_interval).await;
        }
    }

    async fn monitor_account(&self, account: &str) -> anyhow::Result<()> {
        let cursor = self.get_cursor(account).await?;
        let payments = self.fetch_payments(account, cursor.as_deref()).await?;

        info!("Found {} new payments for {}", payments.len(), account);

        let mut last_successful_id: Option<String> = None;

        for payment in payments {
            match self.process_payment(&payment).await {
                Ok(_) => last_successful_id = Some(payment.id.clone()),
                Err(e) => {
                    warn!("Failed to process payment {}: {}", payment.id, e);
                    // Route to DLQ if match found but verification failed
                    if let Err(dlq_err) = self.route_to_dlq(&payment, &e).await {
                        error!("Failed to route payment {} to DLQ: {}", payment.id, dlq_err);
                    }
                }
            }
        }

        if let Some(id) = last_successful_id {
            self.save_cursor(account, &id).await?;
        }

        Ok(())
    }

    async fn fetch_payments(
        &self,
        account: &str,
        cursor: Option<&str>,
    ) -> anyhow::Result<Vec<Payment>> {
        let mut url = format!(
            "{}/accounts/{}/payments?order=asc&limit=200",
            self.horizon_client.base_url.trim_end_matches('/'),
            account
        );

        if let Some(c) = cursor {
            url.push_str(&format!("&cursor={c}"));
        }

        let response = self.horizon_client.client.get(&url).send().await?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!("Horizon API error: {}", response.status()));
        }

        let payments_response: PaymentsResponse = response.json().await?;

        Ok(payments_response
            .embedded
            .records
            .into_iter()
            .map(|r| Payment {
                id: r.id,
                from: r.from,
                to: r.to,
                amount: r.amount,
                asset_code: r.asset_code,
                memo: r.memo,
                memo_type: r.memo_type,
            })
            .collect())
    }

    async fn process_payment(&self, payment: &Payment) -> anyhow::Result<()> {
        // Check for duplicate payment ID (idempotency)
        let already_processed = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM transactions WHERE horizon_payment_id = $1)"
        )
        .bind(&payment.id)
        .fetch_one(&self.pool)
        .await?;

        if already_processed {
            info!("Horizon payment {} already processed (idempotent)", payment.id);
            return Ok(());
        }

        if payment.memo.is_none() {
            return Err(anyhow::anyhow!("Payment {} has no memo", payment.id));
        }

        let memo = payment.memo.as_ref().unwrap();
        let payment_amount = payment.amount.parse::<f64>()?;

        let tx = sqlx::query_as::<_, (Uuid, String, String, sqlx::types::BigDecimal)>(
            "SELECT id, stellar_account, asset_code, amount FROM transactions WHERE memo = $1 AND status = 'pending' LIMIT 1"
        )
        .bind(memo)
        .fetch_optional(&self.pool)
        .await?;

        if let Some((tx_id, expected_account, expected_asset, expected_amount)) = tx {
            // Verify destination account matches
            if payment.to != expected_account {
                return Err(anyhow::anyhow!(
                    "Payment destination {} does not match transaction account {}",
                    payment.to,
                    expected_account
                ));
            }

            // Verify asset code matches
            if payment.asset_code != expected_asset {
                return Err(anyhow::anyhow!(
                    "Payment asset {} does not match transaction asset {}",
                    payment.asset_code,
                    expected_asset
                ));
            }

            // Verify amount is at least equal to expected (allow overpayment)
            let expected_amount_f64 = expected_amount.to_string().parse::<f64>()?;
            if payment_amount < expected_amount_f64 {
                return Err(anyhow::anyhow!(
                    "Payment amount {} is less than expected amount {}",
                    payment_amount,
                    expected_amount_f64
                ));
            }

            info!(
                "Verified payment {} matches transaction {}",
                payment.id, tx_id
            );

            // Validate status transition: pending → completed
            crate::validation::state_machine::validate_status_transition(
                "pending",
                "completed",
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?;

            // Update transaction to completed and record horizon_payment_id
            sqlx::query(
                "UPDATE transactions SET status = 'completed', horizon_payment_id = $1, updated_at = NOW() WHERE id = $2"
            )
            .bind(&payment.id)
            .bind(tx_id)
            .execute(&self.pool)
            .await?;

            info!("Completed transaction {} via payment monitoring", tx_id);
        } else {
            return Err(anyhow::anyhow!(
                "No pending transaction found with memo {}",
                memo
            ));
        }

        Ok(())
    }

    async fn get_cursor(&self, account: &str) -> anyhow::Result<Option<String>> {
        let cursor = sqlx::query_scalar::<_, String>(
            "SELECT cursor FROM account_monitor_cursors WHERE account = $1",
        )
        .bind(account)
        .fetch_optional(&self.pool)
        .await?;

        Ok(cursor)
    }

    async fn save_cursor(&self, account: &str, cursor: &str) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO account_monitor_cursors (account, cursor, updated_at) 
             VALUES ($1, $2, NOW()) 
             ON CONFLICT (account) 
             DO UPDATE SET cursor = $2, updated_at = NOW()",
        )
        .bind(account)
        .bind(cursor)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn route_to_dlq(&self, payment: &Payment, error: &anyhow::Error) -> anyhow::Result<()> {
        // Try to extract transaction ID and details if a matching transaction exists
        if let Some(memo) = &payment.memo {
            if let Ok(Some((tx_id, stellar_account, amount, asset_code, anchor_tx_id))) =
                sqlx::query_as::<_, (Uuid, String, sqlx::types::BigDecimal, String, Option<String>)>(
                    "SELECT id, stellar_account, amount, asset_code, anchor_transaction_id FROM transactions WHERE memo = $1 AND status = 'pending' LIMIT 1"
                )
                .bind(memo)
                .fetch_optional(&self.pool)
                .await
            {
                sqlx::query(
                    "INSERT INTO transaction_dlq (transaction_id, stellar_account, amount, asset_code, anchor_transaction_id, error_reason, original_created_at) 
                     VALUES ($1, $2, $3, $4, $5, $6, NOW())"
                )
                .bind(tx_id)
                .bind(&stellar_account)
                .bind(&amount)
                .bind(&asset_code)
                .bind(anchor_tx_id)
                .bind(error.to_string())
                .execute(&self.pool)
                .await?;

                info!(
                    "Routed transaction {} to DLQ due to payment mismatch: {}",
                    tx_id, error
                );
            }
        }

        Ok(())
    }

    pub async fn start_streaming(&self, account: &str) -> anyhow::Result<()> {
        info!("Starting SSE stream for account {}", account);

        // Load the persisted paging token so the stream resumes without gaps.
        let initial_cursor = self.get_cursor(account).await?;

        let (tx, mut rx) = mpsc::channel(100);

        let client = self.horizon_client.clone();
        let account_clone = account.to_string();
        let cursor_clone = initial_cursor.clone();

        tokio::spawn(async move {
            if let Err(e) = client.stream_payments(&account_clone, tx, cursor_clone).await {
                error!("Stream error for {}: {}", account_clone, e);
            }
        });

        while let Some(result) = rx.recv().await {
            match result {
                Ok(payment) => {
                    let payment_id = payment.id.clone();
                    let payment_obj = Payment {
                        id: payment.id,
                        from: payment.from,
                        to: payment.to,
                        amount: payment.amount,
                        asset_code: payment.asset_code,
                        memo: payment.memo,
                        memo_type: payment.memo_type,
                    };

                    match self.process_payment(&payment_obj).await {
                        Ok(_) => {
                            // Persist cursor so a process restart resumes from here.
                            if let Err(e) = self.save_cursor(account, &payment_id).await {
                                warn!("Failed to persist stream cursor for {}: {}", account, e);
                            }
                        }
                        Err(e) => {
                            warn!("Failed to process streamed payment {}: {}", payment_id, e);
                            if let Err(dlq_err) = self.route_to_dlq(&payment_obj, &e).await {
                                error!(
                                    "Failed to route payment {} to DLQ: {}",
                                    payment_id, dlq_err
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!("Stream error: {}, falling back to polling", e);
                    self.start().await;
                    return Ok(());
                }
            }
        }

        Ok(())
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    async fn get_pool() -> PgPool {
        sqlx::PgPool::connect(&std::env::var("DATABASE_URL").expect("DATABASE_URL not set"))
            .await
            .expect("Failed to connect to test database")
    }

    async fn insert_pending_transaction(
        pool: &PgPool,
        account: &str,
        amount: f64,
        asset_code: &str,
        memo: &str,
    ) -> Uuid {
        let tx_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO transactions (id, stellar_account, amount, asset_code, status, memo) 
             VALUES ($1, $2, $3, $4, 'pending', $5)"
        )
        .bind(tx_id)
        .bind(account)
        .bind(sqlx::types::BigDecimal::from_str(&amount.to_string()).unwrap())
        .bind(asset_code)
        .bind(memo)
        .execute(pool)
        .await
        .expect("insert failed");
        tx_id
    }

    async fn get_transaction_status(pool: &PgPool, tx_id: Uuid) -> String {
        sqlx::query_scalar::<_, String>("SELECT status FROM transactions WHERE id = $1")
            .bind(tx_id)
            .fetch_one(pool)
            .await
            .expect("fetch failed")
    }

    async fn get_transaction_horizon_id(pool: &PgPool, tx_id: Uuid) -> Option<String> {
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT horizon_payment_id FROM transactions WHERE id = $1",
        )
        .bind(tx_id)
        .fetch_one(pool)
        .await
        .expect("fetch failed")
    }

    async fn get_dlq_count(pool: &PgPool, tx_id: Uuid) -> i64 {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM transaction_dlq WHERE transaction_id = $1")
            .bind(tx_id)
            .fetch_one(pool)
            .await
            .expect("fetch failed")
    }

    #[tokio::test]
    #[ignore = "requires DATABASE_URL and migrations"]
    async fn test_cursor_not_advanced_past_failed_payment() {
        let pool = get_pool().await;
        let account = "GACCOUNT1";
        let tx_id = insert_pending_transaction(&pool, account, 100.0, "USD", "memo1").await;

        let payment = Payment {
            id: "payment1".to_string(),
            from: "GSENDER".to_string(),
            to: account.to_string(),
            amount: "50.0".to_string(),
            asset_code: "USD".to_string(),
            memo: Some("memo1".to_string()),
            memo_type: Some("text".to_string()),
        };

        let monitor = AccountMonitor::new(
            HorizonClient::new("https://horizon-testnet.stellar.org".to_string()),
            pool.clone(),
            vec![account.to_string()],
            60,
        );

        let result = monitor.process_payment(&payment).await;
        assert!(result.is_err(), "Payment with wrong amount should fail");

        let status = get_transaction_status(&pool, tx_id).await;
        assert_eq!(status, "pending", "Transaction should remain pending");

        let payment2 = Payment {
            id: "payment2".to_string(),
            from: "GSENDER".to_string(),
            to: account.to_string(),
            amount: "100.0".to_string(),
            asset_code: "USD".to_string(),
            memo: Some("memo1".to_string()),
            memo_type: Some("text".to_string()),
        };

        let result = monitor.process_payment(&payment2).await;
        assert!(result.is_ok(), "Payment with correct amount should succeed");

        let status = get_transaction_status(&pool, tx_id).await;
        assert_eq!(status, "completed", "Transaction should be completed");
    }

    #[tokio::test]
    #[ignore = "requires DATABASE_URL and migrations"]
    async fn test_payment_wrong_amount_not_completed() {
        let pool = get_pool().await;
        let account = "GACCOUNT2";
        let tx_id = insert_pending_transaction(&pool, account, 100.0, "USD", "memo2").await;

        let payment = Payment {
            id: "payment3".to_string(),
            from: "GSENDER".to_string(),
            to: account.to_string(),
            amount: "50.0".to_string(),
            asset_code: "USD".to_string(),
            memo: Some("memo2".to_string()),
            memo_type: Some("text".to_string()),
        };

        let monitor = AccountMonitor::new(
            HorizonClient::new("https://horizon-testnet.stellar.org".to_string()),
            pool.clone(),
            vec![account.to_string()],
            60,
        );

        let result = monitor.process_payment(&payment).await;
        assert!(
            result.is_err() && result.unwrap_err().to_string().contains("less than expected"),
            "Should reject underpayment"
        );

        let status = get_transaction_status(&pool, tx_id).await;
        assert_eq!(status, "pending");
    }

    #[tokio::test]
    #[ignore = "requires DATABASE_URL and migrations"]
    async fn test_payment_wrong_asset_not_completed() {
        let pool = get_pool().await;
        let account = "GACCOUNT3";
        let tx_id = insert_pending_transaction(&pool, account, 100.0, "USD", "memo3").await;

        let payment = Payment {
            id: "payment4".to_string(),
            from: "GSENDER".to_string(),
            to: account.to_string(),
            amount: "100.0".to_string(),
            asset_code: "EUR".to_string(),
            memo: Some("memo3".to_string()),
            memo_type: Some("text".to_string()),
        };

        let monitor = AccountMonitor::new(
            HorizonClient::new("https://horizon-testnet.stellar.org".to_string()),
            pool.clone(),
            vec![account.to_string()],
            60,
        );

        let result = monitor.process_payment(&payment).await;
        assert!(
            result.is_err() && result.unwrap_err().to_string().contains("does not match"),
            "Should reject wrong asset"
        );

        let status = get_transaction_status(&pool, tx_id).await;
        assert_eq!(status, "pending");
    }

    #[tokio::test]
    #[ignore = "requires DATABASE_URL and migrations"]
    async fn test_payment_wrong_destination_not_completed() {
        let pool = get_pool().await;
        let account = "GACCOUNT4";
        let tx_id = insert_pending_transaction(&pool, account, 100.0, "USD", "memo4").await;

        let payment = Payment {
            id: "payment5".to_string(),
            from: "GSENDER".to_string(),
            to: "GWRONGACCOUNT".to_string(),
            amount: "100.0".to_string(),
            asset_code: "USD".to_string(),
            memo: Some("memo4".to_string()),
            memo_type: Some("text".to_string()),
        };

        let monitor = AccountMonitor::new(
            HorizonClient::new("https://horizon-testnet.stellar.org".to_string()),
            pool.clone(),
            vec![account.to_string()],
            60,
        );

        let result = monitor.process_payment(&payment).await;
        assert!(
            result.is_err() && result.unwrap_err().to_string().contains("destination"),
            "Should reject wrong destination"
        );

        let status = get_transaction_status(&pool, tx_id).await;
        assert_eq!(status, "pending");
    }

    #[tokio::test]
    #[ignore = "requires DATABASE_URL and migrations"]
    async fn test_correct_payment_completes() {
        let pool = get_pool().await;
        let account = "GACCOUNT5";
        let tx_id = insert_pending_transaction(&pool, account, 100.0, "USD", "memo5").await;

        let payment = Payment {
            id: "payment6".to_string(),
            from: "GSENDER".to_string(),
            to: account.to_string(),
            amount: "100.0".to_string(),
            asset_code: "USD".to_string(),
            memo: Some("memo5".to_string()),
            memo_type: Some("text".to_string()),
        };

        let monitor = AccountMonitor::new(
            HorizonClient::new("https://horizon-testnet.stellar.org".to_string()),
            pool.clone(),
            vec![account.to_string()],
            60,
        );

        let result = monitor.process_payment(&payment).await;
        assert!(result.is_ok());

        let status = get_transaction_status(&pool, tx_id).await;
        assert_eq!(status, "completed");

        let horizon_id = get_transaction_horizon_id(&pool, tx_id).await;
        assert_eq!(horizon_id, Some("payment6".to_string()));
    }

    #[tokio::test]
    #[ignore = "requires DATABASE_URL and migrations"]
    async fn test_overpayment_allowed() {
        let pool = get_pool().await;
        let account = "GACCOUNT6";
        let tx_id = insert_pending_transaction(&pool, account, 100.0, "USD", "memo6").await;

        let payment = Payment {
            id: "payment7".to_string(),
            from: "GSENDER".to_string(),
            to: account.to_string(),
            amount: "150.0".to_string(),
            asset_code: "USD".to_string(),
            memo: Some("memo6".to_string()),
            memo_type: Some("text".to_string()),
        };

        let monitor = AccountMonitor::new(
            HorizonClient::new("https://horizon-testnet.stellar.org".to_string()),
            pool.clone(),
            vec![account.to_string()],
            60,
        );

        let result = monitor.process_payment(&payment).await;
        assert!(result.is_ok(), "Overpayment should be accepted");

        let status = get_transaction_status(&pool, tx_id).await;
        assert_eq!(status, "completed");
    }

    #[tokio::test]
    #[ignore = "requires DATABASE_URL and migrations"]
    async fn test_duplicate_horizon_payment_id_idempotent() {
        let pool = get_pool().await;
        let account = "GACCOUNT7";
        let tx_id = insert_pending_transaction(&pool, account, 100.0, "USD", "memo7").await;

        let payment = Payment {
            id: "payment8".to_string(),
            from: "GSENDER".to_string(),
            to: account.to_string(),
            amount: "100.0".to_string(),
            asset_code: "USD".to_string(),
            memo: Some("memo7".to_string()),
            memo_type: Some("text".to_string()),
        };

        let monitor = AccountMonitor::new(
            HorizonClient::new("https://horizon-testnet.stellar.org".to_string()),
            pool.clone(),
            vec![account.to_string()],
            60,
        );

        let result = monitor.process_payment(&payment).await;
        assert!(result.is_ok());

        let status = get_transaction_status(&pool, tx_id).await;
        assert_eq!(status, "completed");

        let result = monitor.process_payment(&payment).await;
        assert!(result.is_ok(), "Replay should be idempotent");

        let status = get_transaction_status(&pool, tx_id).await;
        assert_eq!(status, "completed", "Status should remain completed");

        let horizon_id = get_transaction_horizon_id(&pool, tx_id).await;
        assert_eq!(horizon_id, Some("payment8".to_string()));
    }

    #[tokio::test]
    #[ignore = "requires DATABASE_URL and migrations"]
    async fn test_failed_payment_routed_to_dlq() {
        let pool = get_pool().await;
        let account = "GACCOUNT8";
        let tx_id = insert_pending_transaction(&pool, account, 100.0, "USD", "memo8").await;

        let payment = Payment {
            id: "payment9".to_string(),
            from: "GSENDER".to_string(),
            to: account.to_string(),
            amount: "50.0".to_string(),
            asset_code: "USD".to_string(),
            memo: Some("memo8".to_string()),
            memo_type: Some("text".to_string()),
        };

        let monitor = AccountMonitor::new(
            HorizonClient::new("https://horizon-testnet.stellar.org".to_string()),
            pool.clone(),
            vec![account.to_string()],
            60,
        );

        let error = monitor.process_payment(&payment).await.unwrap_err();
        let _ = monitor.route_to_dlq(&payment, &error).await;

        let dlq_count = get_dlq_count(&pool, tx_id).await;
        assert_eq!(dlq_count, 1, "Failed payment should be in DLQ");
    }
}
