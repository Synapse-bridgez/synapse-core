use crate::stellar::client::HorizonClient;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::time::Duration;
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

        let last_id = payments.last().map(|p| p.id.clone());

        for payment in payments {
            if let Err(e) = self.process_payment(&payment).await {
                warn!("Failed to process payment {}: {}", payment.id, e);
            }
        }

        if let Some(id) = last_id {
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
            url.push_str(&format!("&cursor={}", c));
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
        // Match payment to pending transaction by memo
        if let Some(memo) = &payment.memo {
            let tx = sqlx::query_as::<_, (Uuid,)>(
                "SELECT id FROM transactions WHERE memo = $1 AND status = 'pending' LIMIT 1",
            )
            .bind(memo)
            .fetch_optional(&self.pool)
            .await?;

            if let Some((tx_id,)) = tx {
                info!("Matched payment {} to transaction {}", payment.id, tx_id);

                // Update transaction to completed
                sqlx::query(
                    "UPDATE transactions SET status = 'completed', updated_at = NOW() WHERE id = $1"
                )
                .bind(tx_id)
                .execute(&self.pool)
                .await?;

                info!("Completed transaction {} via payment monitoring", tx_id);
            }
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
}
