use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use sqlx::types::BigDecimal;
use uuid::Uuid;
use utoipa::ToSchema;

#[derive(Debug, FromRow, Serialize, Deserialize)]
pub struct Transaction {
    pub id: Uuid,
    pub stellar_account: String,
    #[serde(with = "bigdecimal_serde")]
    pub amount: BigDecimal, // now available
    pub asset_code: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub anchor_transaction_id: Option<String>,
    pub callback_type: Option<String>,
    pub callback_status: Option<String>,
    pub settlement_id: Option<Uuid>,
}

impl Transaction {
    pub fn new(
        stellar_account: String,
        amount: BigDecimal,
        asset_code: String,
        anchor_transaction_id: Option<String>,
        callback_type: Option<String>,
        callback_status: Option<String>,
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
        }
    }
}

#[derive(Debug, FromRow, Serialize, Deserialize)]
pub struct Settlement {
    pub id: Uuid,
    pub asset_code: String,
    #[serde(with = "bigdecimal_serde")]
    pub total_amount: BigDecimal,
    pub tx_count: i32,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, FromRow, Serialize, Deserialize)]
pub struct TransactionDlq {
    pub id: Uuid,
    pub transaction_id: Uuid,
    pub stellar_account: String,
    #[serde(with = "bigdecimal_serde")]
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
    use sqlx::PgPool;
    use sqlx::migrate::Migrator;
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
        );

        sqlx::query!(
            r#"
            INSERT INTO transactions (
                id, stellar_account, amount, asset_code, status,
                created_at, updated_at, anchor_transaction_id, callback_type, callback_status
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            "#,
            tx.id,
            tx.stellar_account,
            tx.amount,
            tx.asset_code,
            tx.status,
            tx.created_at,
            tx.updated_at,
            tx.anchor_transaction_id,
            tx.callback_type,
            tx.callback_status,
        )
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
        let pool = PgPool::connect("postgres://user:password@localhost/test_db").await.unwrap();
        let tx = Transaction::new(
            "GABCDEF".to_string(),
            BigDecimal::from(100),
            "USD".to_string(),
            None,
            None,
            None,
        );
        let inserted = crate::db::queries::insert_transaction(&pool, &tx).await.unwrap();
        assert_eq!(inserted.stellar_account, tx.stellar_account);
    }

    #[tokio::test]
    async fn test_get_transaction() {
        let pool = PgPool::connect("postgres://user:password@localhost/test_db").await.unwrap();
        let tx = Transaction::new(
            "GABCDEF".to_string(),
            BigDecimal::from(100),
            "USD".to_string(),
            None,
            None,
            None,
        );
        let inserted = crate::db::queries::insert_transaction(&pool, &tx).await.unwrap();
        let fetched = crate::db::queries::get_transaction(&pool, inserted.id).await.unwrap();
        assert_eq!(fetched.id, inserted.id);
    }

    #[tokio::test]
    async fn test_list_transactions() {
        let pool = PgPool::connect("postgres://user:password@localhost/test_db").await.unwrap();
        for i in 0..5 {
            let tx = Transaction::new(
                format!("GABCDEF_{}", i),
                BigDecimal::from(100 + i),
                "USD".to_string(),
                None,
                None,
                None,
            );
            crate::db::queries::insert_transaction(&pool, &tx).await.unwrap();
        }
        let transactions = crate::db::queries::list_transactions(&pool, 5, 0).await.unwrap();
        assert_eq!(transactions.len(), 5);
    }
}

mod bigdecimal_serde {
    use serde::{Deserialize, Deserializer, Serializer};
    use sqlx::types::BigDecimal;
    use std::str::FromStr;

    pub fn serialize<S>(value: &BigDecimal, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&value.to_string())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<BigDecimal, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        BigDecimal::from_str(&s).map_err(serde::de::Error::custom)
    }
}

