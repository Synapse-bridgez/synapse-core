use bigdecimal::BigDecimal;
use chrono::{Duration, Utc};
use sqlx::{migrate::Migrator, PgPool};
use std::path::Path;
use synapse_core::db::models::Transaction;
use synapse_core::error::AppError;
use synapse_core::services::SettlementService;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;

async fn setup_test_db() -> (PgPool, impl std::any::Any) {
    let container = Postgres::default().start().await.unwrap();
    let host_port = container.get_host_port_ipv4(5432).await.unwrap();
    let database_url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        host_port
    );

    let pool = PgPool::connect(&database_url).await.unwrap();
    let migrator = Migrator::new(Path::join(
        Path::new(env!("CARGO_MANIFEST_DIR")),
        "migrations",
    ))
    .await
    .unwrap();
    migrator.run(&pool).await.unwrap();

    (pool, container)
}

/// Helper that inserts a transaction row directly; we avoid using the
/// potentially-buggy `queries::insert_transaction` to keep the tests
/// self-contained.
async fn insert_tx(pool: &PgPool, tx: &Transaction) -> Transaction {
    sqlx::query_as::<_, Transaction>(
        r#"
        INSERT INTO transactions (
            id, stellar_account, amount, asset_code, status,
            created_at, updated_at, anchor_transaction_id, callback_type, callback_status,
            settlement_id, memo, memo_type, metadata
        ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)
        RETURNING *
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
    .bind(tx.settlement_id)
    .bind(&tx.memo)
    .bind(&tx.memo_type)
    .bind(&tx.metadata)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn test_settle_single_asset() {
    let (pool, _container) = setup_test_db().await;
    let service = SettlementService::new(pool.clone());

    let mut tx = Transaction::new(
        "GA111111111111111111111111111111111111111111111111".to_string(),
        BigDecimal::from(100),
        "USD".to_string(),
        None,
        None,
        None,
        None,
        None,
        None,
    );
    tx.status = "completed".to_string();
    let inserted = insert_tx(&pool, &tx).await;

    let result = service.settle_asset("USD").await.unwrap();
    assert!(result.is_some());
    let settlement = result.unwrap();
    assert_eq!(settlement.asset_code, "USD");
    assert_eq!(settlement.tx_count, 1);
    assert_eq!(settlement.total_amount, BigDecimal::from(100));

    let updated_tx: Transaction = sqlx::query_as("SELECT * FROM transactions WHERE id = $1")
        .bind(inserted.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(updated_tx.settlement_id, Some(settlement.id));
}

#[tokio::test]
async fn test_settle_multiple_transactions() {
    let (pool, _container) = setup_test_db().await;
    let service = SettlementService::new(pool.clone());

    let now = Utc::now();
    let earlier = now - Duration::hours(2);
    let middle = now - Duration::hours(1);

    let mut tx1 = Transaction::new(
        "GBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB".to_string(),
        BigDecimal::from(75),
        "EUR".to_string(),
        None,
        None,
        None,
        None,
        None,
        None,
    );
    tx1.status = "completed".to_string();
    tx1.created_at = earlier;
    tx1.updated_at = middle;

    let mut tx2 = Transaction::new(
        "GCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC".to_string(),
        BigDecimal::from(25),
        "EUR".to_string(),
        None,
        None,
        None,
        None,
        None,
        None,
    );
    tx2.status = "completed".to_string();
    tx2.created_at = middle;
    tx2.updated_at = now;

    let inserted1 = insert_tx(&pool, &tx1).await;
    let inserted2 = insert_tx(&pool, &tx2).await;

    let settlement = service.settle_asset("EUR").await.unwrap().unwrap();
    assert_eq!(settlement.tx_count, 2);
    assert_eq!(settlement.total_amount, BigDecimal::from(100));
    assert_eq!(settlement.period_start, earlier);
    assert_eq!(settlement.period_end, now);

    // ensure both transactions were updated
    let u1: Transaction = sqlx::query_as("SELECT * FROM transactions WHERE id=$1")
        .bind(inserted1.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let u2: Transaction = sqlx::query_as("SELECT * FROM transactions WHERE id=$1")
        .bind(inserted2.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(u1.settlement_id, Some(settlement.id));
    assert_eq!(u2.settlement_id, Some(settlement.id));
}

#[tokio::test]
async fn test_settle_no_unsettled_transactions() {
    let (pool, _container) = setup_test_db().await;
    let service = SettlementService::new(pool.clone());

    let result = service.settle_asset("NONEXISTENT").await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn test_settle_error_handling() {
    let (pool, _container) = setup_test_db().await;
    let service = SettlementService::new(pool.clone());

    // cause a database error by dropping the table before the call
    sqlx::query("DROP TABLE transactions")
        .execute(&pool)
        .await
        .unwrap();

    let err = service.settle_asset("USD").await;
    assert!(matches!(err, Err(AppError::DatabaseError(_))));
}

#[tokio::test]
async fn test_asset_grouping() {
    let (pool, _container) = setup_test_db().await;
    let service = SettlementService::new(pool.clone());

    // insert a completed USD transaction
    let mut usd = Transaction::new(
        "GDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD".to_string(),
        BigDecimal::from(40),
        "USD".to_string(),
        None,
        None,
        None,
        None,
        None,
        None,
    );
    usd.status = "completed".to_string();
    insert_tx(&pool, &usd).await;

    // insert a completed EUR transaction
    let mut eur = Transaction::new(
        "GEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE".to_string(),
        BigDecimal::from(60),
        "EUR".to_string(),
        None,
        None,
        None,
        None,
        None,
        None,
    );
    eur.status = "completed".to_string();
    insert_tx(&pool, &eur).await;

    // a pending GBP transaction shouldn't be settled
    let mut gbp = Transaction::new(
        "GFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF".to_string(),
        BigDecimal::from(10),
        "GBP".to_string(),
        None,
        None,
        None,
        None,
        None,
        None,
    );
    gbp.status = "pending".to_string();
    insert_tx(&pool, &gbp).await;

    let results = service.run_settlements().await.unwrap();
    assert_eq!(results.len(), 2);
    let assets: Vec<_> = results.iter().map(|s| s.asset_code.as_str()).collect();
    assert!(assets.contains(&"USD"));
    assert!(assets.contains(&"EUR"));
}
