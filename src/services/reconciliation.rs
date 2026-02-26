use crate::stellar::client::HorizonClient;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use tracing::{info, warn};
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize)]
pub struct ReconciliationReport {
    pub generated_at: DateTime<Utc>,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub total_db_transactions: usize,
    pub total_chain_payments: usize,
    pub missing_on_chain: Vec<MissingTransaction>,
    pub orphaned_payments: Vec<OrphanedPayment>,
    pub amount_mismatches: Vec<AmountMismatch>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MissingTransaction {
    pub id: Uuid,
    pub stellar_account: String,
    pub amount: String,
    pub asset_code: String,
    pub memo: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OrphanedPayment {
    pub payment_id: String,
    pub from: String,
    pub to: String,
    pub amount: String,
    pub asset_code: String,
    pub memo: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AmountMismatch {
    pub transaction_id: Uuid,
    pub payment_id: String,
    pub db_amount: String,
    pub chain_amount: String,
    pub memo: Option<String>,
}

#[derive(Debug)]
struct DbTransaction {
    id: Uuid,
    stellar_account: String,
    amount: String,
    asset_code: String,
    memo: Option<String>,
    created_at: DateTime<Utc>,
}

#[derive(Debug)]
struct ChainPayment {
    id: String,
    from: String,
    to: String,
    amount: String,
    asset_code: String,
    memo: Option<String>,
}

pub struct ReconciliationService {
    horizon_client: HorizonClient,
    pool: PgPool,
}

impl ReconciliationService {
    pub fn new(horizon_client: HorizonClient, pool: PgPool) -> Self {
        Self {
            horizon_client,
            pool,
        }
    }

    pub async fn reconcile(
        &self,
        account: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> anyhow::Result<ReconciliationReport> {
        info!(
            "Starting reconciliation for {} from {} to {}",
            account, start, end
        );

        // Fetch DB transactions
        let db_txs = self.fetch_db_transactions(account, start, end).await?;
        info!("Found {} transactions in database", db_txs.len());

        // Fetch chain payments
        let chain_payments = self.fetch_chain_payments(account, start, end).await?;
        info!("Found {} payments on chain", chain_payments.len());

        // Build lookup maps
        let mut db_by_memo: HashMap<String, &DbTransaction> = HashMap::new();
        let mut chain_by_memo: HashMap<String, &ChainPayment> = HashMap::new();

        for tx in &db_txs {
            if let Some(memo) = &tx.memo {
                db_by_memo.insert(memo.clone(), tx);
            }
        }

        for payment in &chain_payments {
            if let Some(memo) = &payment.memo {
                chain_by_memo.insert(memo.clone(), payment);
            }
        }

        // Find discrepancies
        let mut missing_on_chain = Vec::new();
        let mut amount_mismatches = Vec::new();

        for tx in &db_txs {
            if let Some(memo) = &tx.memo {
                if let Some(payment) = chain_by_memo.get(memo) {
                    // Check amount match
                    if tx.amount != payment.amount {
                        amount_mismatches.push(AmountMismatch {
                            transaction_id: tx.id,
                            payment_id: payment.id.clone(),
                            db_amount: tx.amount.clone(),
                            chain_amount: payment.amount.clone(),
                            memo: Some(memo.clone()),
                        });
                    }
                } else {
                    // Transaction in DB but not on chain
                    missing_on_chain.push(MissingTransaction {
                        id: tx.id,
                        stellar_account: tx.stellar_account.clone(),
                        amount: tx.amount.clone(),
                        asset_code: tx.asset_code.clone(),
                        memo: tx.memo.clone(),
                        created_at: tx.created_at,
                    });
                }
            }
        }

        // Find orphaned payments
        let mut orphaned_payments = Vec::new();
        let db_memos: HashSet<_> = db_by_memo.keys().collect();

        for payment in &chain_payments {
            if let Some(memo) = &payment.memo {
                if !db_memos.contains(memo) {
                    orphaned_payments.push(OrphanedPayment {
                        payment_id: payment.id.clone(),
                        from: payment.from.clone(),
                        to: payment.to.clone(),
                        amount: payment.amount.clone(),
                        asset_code: payment.asset_code.clone(),
                        memo: Some(memo.clone()),
                    });
                }
            }
        }

        let report = ReconciliationReport {
            generated_at: Utc::now(),
            period_start: start,
            period_end: end,
            total_db_transactions: db_txs.len(),
            total_chain_payments: chain_payments.len(),
            missing_on_chain,
            orphaned_payments,
            amount_mismatches,
        };

        info!(
            "Reconciliation complete: {} missing, {} orphaned, {} mismatches",
            report.missing_on_chain.len(),
            report.orphaned_payments.len(),
            report.amount_mismatches.len()
        );

        Ok(report)
    }

    async fn fetch_db_transactions(
        &self,
        account: &str,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> anyhow::Result<Vec<DbTransaction>> {
        let rows =
            sqlx::query_as::<_, (Uuid, String, String, String, Option<String>, DateTime<Utc>)>(
                "SELECT id, stellar_account, amount::text, asset_code, memo, created_at 
             FROM transactions 
             WHERE stellar_account = $1 
             AND created_at >= $2 
             AND created_at <= $3 
             AND status = 'completed'
             ORDER BY created_at",
            )
            .bind(account)
            .bind(start)
            .bind(end)
            .fetch_all(&self.pool)
            .await?;

        Ok(rows
            .into_iter()
            .map(
                |(id, stellar_account, amount, asset_code, memo, created_at)| DbTransaction {
                    id,
                    stellar_account,
                    amount,
                    asset_code,
                    memo,
                    created_at,
                },
            )
            .collect())
    }

    async fn fetch_chain_payments(
        &self,
        account: &str,
        _start: DateTime<Utc>,
        _end: DateTime<Utc>,
    ) -> anyhow::Result<Vec<ChainPayment>> {
        let url = format!(
            "{}/accounts/{}/payments?order=asc&limit=200",
            self.horizon_client.base_url.trim_end_matches('/'),
            account
        );

        let response = self.horizon_client.client.get(&url).send().await?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!("Horizon API error: {}", response.status()));
        }

        #[derive(Deserialize)]
        struct PaymentsResponse {
            #[serde(rename = "_embedded")]
            embedded: Embedded,
        }

        #[derive(Deserialize)]
        struct Embedded {
            records: Vec<PaymentRecord>,
        }

        #[derive(Deserialize)]
        struct PaymentRecord {
            id: String,
            from: String,
            to: String,
            amount: String,
            asset_code: String,
            #[serde(default)]
            memo: Option<String>,
        }

        let payments_response: PaymentsResponse = response.json().await?;

        Ok(payments_response
            .embedded
            .records
            .into_iter()
            .map(|r| ChainPayment {
                id: r.id,
                from: r.from,
                to: r.to,
                amount: r.amount,
                asset_code: r.asset_code,
                memo: r.memo,
            })
            .collect())
    }
}
