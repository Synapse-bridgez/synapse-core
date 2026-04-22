use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::types::BigDecimal;
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, FromRow, Serialize, Deserialize, Clone)]
#[serde(rename_all = "snake_case")]
pub struct Transaction {
    pub id: Uuid,
    pub stellar_account: String,
    pub amount: BigDecimal,
    pub asset_code: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub anchor_transaction_id: Option<String>,
    pub callback_type: Option<String>,
    pub callback_status: Option<String>,
    pub settlement_id: Option<Uuid>,
    pub memo: Option<String>,
    pub memo_type: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

#[async_graphql::Object]
impl Transaction {
    async fn id(&self) -> String {
        self.id.to_string()
    }
    async fn stellar_account(&self) -> &str {
        &self.stellar_account
    }
    async fn amount(&self) -> String {
        self.amount.to_string()
    }
    async fn asset_code(&self) -> &str {
        &self.asset_code
    }
    async fn status(&self) -> &str {
        &self.status
    }
    async fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
    async fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
    }
    async fn anchor_transaction_id(&self) -> Option<&str> {
        self.anchor_transaction_id.as_deref()
    }
    async fn callback_type(&self) -> Option<&str> {
        self.callback_type.as_deref()
    }
    async fn callback_status(&self) -> Option<&str> {
        self.callback_status.as_deref()
    }
    async fn settlement_id(&self) -> Option<String> {
        self.settlement_id.map(|id| id.to_string())
    }
    async fn memo(&self) -> Option<&str> {
        self.memo.as_deref()
    }
    async fn memo_type(&self) -> Option<&str> {
        self.memo_type.as_deref()
    }
}

impl Transaction {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        stellar_account: String,
        amount: BigDecimal,
        asset_code: String,
        anchor_transaction_id: Option<String>,
        callback_type: Option<String>,
        callback_status: Option<String>,
        memo: Option<String>,
        memo_type: Option<String>,
        metadata: Option<serde_json::Value>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            stellar_account,
            amount,
            asset_code,
            status: "pending".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            anchor_transaction_id,
            callback_type,
            callback_status,
            settlement_id: None,
            memo,
            memo_type,
            metadata,
        }
    }
}

#[derive(Debug, FromRow, Serialize, Deserialize, Clone)]
pub struct Settlement {
    pub id: Uuid,
    pub asset_code: String,
    pub total_amount: BigDecimal,
    pub tx_count: i32,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[async_graphql::Object]
impl Settlement {
    async fn id(&self) -> String {
        self.id.to_string()
    }
    async fn asset_code(&self) -> &str {
        &self.asset_code
    }
    async fn total_amount(&self) -> String {
        self.total_amount.to_string()
    }
    async fn tx_count(&self) -> i32 {
        self.tx_count
    }
    async fn period_start(&self) -> DateTime<Utc> {
        self.period_start
    }
    async fn period_end(&self) -> DateTime<Utc> {
        self.period_end
    }
    async fn status(&self) -> &str {
        &self.status
    }
    async fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
    async fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
    }
}

#[derive(Debug, FromRow, Serialize, Deserialize)]
pub struct TransactionDlq {
    pub id: Uuid,
    pub transaction_id: Uuid,
    pub stellar_account: String,
    pub amount: BigDecimal,
    pub asset_code: String,
    pub anchor_transaction_id: Option<String>,
    pub error_reason: String,
    pub stack_trace: Option<String>,
    pub retry_count: i32,
    pub original_created_at: DateTime<Utc>,
    pub moved_to_dlq_at: DateTime<Utc>,
    pub last_retry_at: Option<DateTime<Utc>>,
}
#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::migrate::Migrator;
    use sqlx::PgPool;
    use std::path::Path;

    async fn setup_test_db() -> PgPool {
        let database_url =
            std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for tests");
        let pool = PgPool::connect(&database_url)
            .await
            .expect("Failed to connect to test DB");
        let migrator = Migrator::new(Path::new("./migrations"))
            .await
            .expect("Failed to load migrations");
        migrator
            .run(&pool)
            .await
            .expect("Failed to run migrations on test DB");

        // Create partition for current month (ignore if already exists)
        let _ = sqlx::query(
            r#"
            DO $$
            DECLARE
                partition_date DATE;
                partition_name TEXT;
                start_date TEXT;
                end_date TEXT;
            BEGIN
                partition_date := DATE_TRUNC('month', NOW());
                partition_name := 'transactions_y' || TO_CHAR(partition_date, 'YYYY') || 'm' || TO_CHAR(partition_date, 'MM');
                start_date := TO_CHAR(partition_date, 'YYYY-MM-DD');
                end_date := TO_CHAR(partition_date + INTERVAL '1 month', 'YYYY-MM-DD');
                
                IF NOT EXISTS (SELECT 1 FROM pg_class WHERE relname = partition_name) THEN
                    EXECUTE format(
                        'CREATE TABLE %I PARTITION OF transactions FOR VALUES FROM (%L) TO (%L)',
                        partition_name, start_date, end_date
                    );
                END IF;
            END $$;
            "#
        )
        .execute(&pool)
        .await;

        pool
    }

    #[tokio::test]
    async fn test_insert_and_query_transaction() {
        let pool = setup_test_db().await;

        let stellar_account = "GABCD1234...".to_string();
        // Create BigDecimal from string to avoid floating-point issues
        let amount = "100.50".parse::<BigDecimal>().unwrap();
        let asset_code = "USD".to_string();
        let anchor_tx_id = Some("anchor-123".to_string());
        let callback_type = Some("deposit".to_string());
        let callback_status = Some("completed".to_string());

        let tx = Transaction::new(
            stellar_account.clone(),
            amount.clone(),
            asset_code.clone(),
            anchor_tx_id.clone(),
            callback_type.clone(),
            callback_status.clone(),
            Some("test memo".to_string()),
            Some("text".to_string()),
            Some(serde_json::json!({"ref": "ABC-123"})),
        );

        sqlx::query(
            r#"
            INSERT INTO transactions (
                id, stellar_account, amount, asset_code, status,
                created_at, updated_at, anchor_transaction_id, callback_type, callback_status,
                memo, memo_type, metadata
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
            "#,
        )
        .bind(tx.id)
        .bind(&tx.stellar_account)
        .bind(&tx.amount)
        .bind(&tx.asset_code)
        .bind(&tx.status)
        .bind(tx.created_at)
        .bind(tx.updated_at)
        .bind(&tx.anchor_transaction_id)
        .bind(&tx.callback_type)
        .bind(&tx.callback_status)
        .bind(&tx.memo)
        .bind(&tx.memo_type)
        .bind(&tx.metadata)
        .execute(&pool)
        .await
        .expect("Failed to insert transaction");

        let fetched = sqlx::query_as::<_, Transaction>("SELECT * FROM transactions WHERE id = $1")
            .bind(tx.id)
            .fetch_one(&pool)
            .await
            .expect("Failed to fetch transaction");

        assert_eq!(fetched.stellar_account, stellar_account);
        assert_eq!(fetched.amount, amount);
        assert_eq!(fetched.asset_code, asset_code);
        assert_eq!(fetched.anchor_transaction_id, anchor_tx_id);
        assert_eq!(fetched.callback_type, callback_type);
        assert_eq!(fetched.callback_status, callback_status);
    }

    #[tokio::test]
    async fn test_insert_transaction() {
        let pool = setup_test_db().await;
        let tx = Transaction::new(
            "GABCDEF".to_string(),
            BigDecimal::from(100),
            "USD".to_string(),
            None,
            None,
            None,
            None,
            None,
            None,
        );
        let inserted = crate::db::queries::insert_transaction(&pool, &tx)
            .await
            .unwrap();
        assert_eq!(inserted.stellar_account, tx.stellar_account);
    }

    #[tokio::test]
    async fn test_get_transaction() {
        let pool = setup_test_db().await;
        let tx = Transaction::new(
            "GABCDEF".to_string(),
            BigDecimal::from(100),
            "USD".to_string(),
            None,
            None,
            None,
            None,
            None,
            None,
        );
        let inserted = crate::db::queries::insert_transaction(&pool, &tx)
            .await
            .unwrap();
        let fetched = crate::db::queries::get_transaction(&pool, inserted.id)
            .await
            .unwrap();
        assert_eq!(fetched.id, inserted.id);
    }

    #[tokio::test]
    async fn test_list_transactions() {
        let pool = setup_test_db().await;
        for i in 0..5 {
            let tx = Transaction::new(
                format!("GABCDEF_{}", i),
                BigDecimal::from(100 + i),
                "USD".to_string(),
                None,
                None,
                None,
                None,
                None,
                None,
            );
            crate::db::queries::insert_transaction(&pool, &tx)
                .await
                .unwrap();
        }
        let transactions = crate::db::queries::list_transactions(&pool, 5, None, false)
            .await
            .unwrap();
        assert_eq!(transactions.len(), 5);
    }
}

// Minimal Asset struct for asset cache functionality
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub asset_code: String,
    pub issuer: Option<String>,
}

impl Asset {
    pub async fn fetch_all(_pool: &sqlx::PgPool) -> Result<Vec<Self>, sqlx::Error> {
        // Placeholder implementation - returns empty vec for now
        Ok(vec![])
    }
}
