/// Tests for #279 — Settlement Dispute Resolution Workflow
///
/// Validates:
/// - dispute → review → resolution flow
/// - voided settlement releases transactions back to unsettled
/// - invalid transitions are rejected
/// - status changes are audit-logged
use bigdecimal::BigDecimal;
use sqlx::{migrate::Migrator, PgPool};
use std::path::Path;
use synapse_core::services::SettlementService;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;

#[path = "fixtures.rs"]
mod fixtures;
use fixtures::TransactionFixture;

async fn setup_db() -> (PgPool, impl std::any::Any) {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{}/postgres", port);
    let pool = PgPool::connect(&url).await.unwrap();
    let migrator = Migrator::new(Path::join(Path::new(env!("CARGO_MANIFEST_DIR")), "migrations"))
        .await
        .unwrap();
    migrator.run(&pool).await.unwrap();

    // Create current-month partition
    sqlx::query(r#"
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
                EXECUTE format('CREATE TABLE %I PARTITION OF transactions FOR VALUES FROM (%L) TO (%L)', partition_name, start_date, end_date);
            END IF;
        END $$;
    "#).execute(&pool).await.unwrap();

    (pool, container)
}

async fn insert_tx(pool: &PgPool, tx: &synapse_core::db::models::Transaction) {
    sqlx::query(r#"
        INSERT INTO transactions (id, stellar_account, amount, asset_code, status,
            created_at, updated_at, anchor_transaction_id, callback_type, callback_status,
            settlement_id, memo, memo_type, metadata)
        VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)
    "#)
    .bind(tx.id).bind(&tx.stellar_account).bind(&tx.amount).bind(&tx.asset_code)
    .bind(&tx.status).bind(tx.created_at).bind(tx.updated_at)
    .bind(&tx.anchor_transaction_id).bind(&tx.callback_type).bind(&tx.callback_status)
    .bind(tx.settlement_id).bind(&tx.memo).bind(&tx.memo_type).bind(&tx.metadata)
    .execute(pool).await.unwrap();
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_settlement_dispute_review_resolution_flow() {
    let (pool, _c) = setup_db().await;
    let svc = SettlementService::new(pool.clone());

    let tx = TransactionFixture::new()
        .with_status("completed")
        .with_asset_code("USD")
        .with_amount("200")
        .build();
    insert_tx(&pool, &tx).await;

    // Create settlement
    let settlement = svc.settle_asset("USD").await.unwrap().unwrap();
    assert_eq!(settlement.status, "completed");

    // completed → pending_review
    let s = svc.update_status(settlement.id, "pending_review", Some("needs review"), None, "admin").await.unwrap();
    assert_eq!(s.status, "pending_review");

    // pending_review → disputed
    let s = svc.update_status(settlement.id, "disputed", Some("amount mismatch"), None, "admin").await.unwrap();
    assert_eq!(s.status, "disputed");

    // disputed → adjusted (with new total)
    let new_total: BigDecimal = "180".parse().unwrap();
    let s = svc.update_status(settlement.id, "adjusted", Some("corrected amount"), Some(&new_total), "admin").await.unwrap();
    assert_eq!(s.status, "adjusted");
    assert_eq!(s.total_amount, new_total);
    // original amount preserved
    assert_eq!(s.original_total_amount.unwrap(), "200".parse::<BigDecimal>().unwrap());
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_voided_settlement_releases_transactions() {
    let (pool, _c) = setup_db().await;
    let svc = SettlementService::new(pool.clone());

    let tx = TransactionFixture::new()
        .with_status("completed")
        .with_asset_code("EUR")
        .with_amount("50")
        .build();
    let tx_id = tx.id;
    insert_tx(&pool, &tx).await;

    let settlement = svc.settle_asset("EUR").await.unwrap().unwrap();

    // Verify transaction is linked
    let linked: (Option<Uuid>,) = sqlx::query_as("SELECT settlement_id FROM transactions WHERE id = $1")
        .bind(tx_id).fetch_one(&pool).await.unwrap();
    assert_eq!(linked.0, Some(settlement.id));

    // completed → pending_review → voided
    svc.update_status(settlement.id, "pending_review", None, None, "admin").await.unwrap();
    svc.update_status(settlement.id, "voided", Some("duplicate settlement"), None, "admin").await.unwrap();

    // Transaction should be released (settlement_id = NULL)
    let released: (Option<Uuid>,) = sqlx::query_as("SELECT settlement_id FROM transactions WHERE id = $1")
        .bind(tx_id).fetch_one(&pool).await.unwrap();
    assert!(released.0.is_none(), "voided settlement should release transactions");
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_invalid_settlement_transition_rejected() {
    let (pool, _c) = setup_db().await;
    let svc = SettlementService::new(pool.clone());

    let tx = TransactionFixture::new()
        .with_status("completed")
        .with_asset_code("GBP")
        .with_amount("75")
        .build();
    insert_tx(&pool, &tx).await;

    let settlement = svc.settle_asset("GBP").await.unwrap().unwrap();

    // completed → adjusted is not a valid direct transition
    let result = svc.update_status(settlement.id, "adjusted", None, None, "admin").await;
    assert!(result.is_err(), "direct completed→adjusted should be rejected");
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_settlement_status_change_is_audit_logged() {
    let (pool, _c) = setup_db().await;
    let svc = SettlementService::new(pool.clone());

    let tx = TransactionFixture::new()
        .with_status("completed")
        .with_asset_code("JPY")
        .with_amount("1000")
        .build();
    insert_tx(&pool, &tx).await;

    let settlement = svc.settle_asset("JPY").await.unwrap().unwrap();
    svc.update_status(settlement.id, "pending_review", Some("audit test"), None, "test_actor").await.unwrap();

    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM audit_logs WHERE entity_id = $1 AND action = 'status_update'"
    )
    .bind(settlement.id)
    .fetch_one(&pool).await.unwrap();

    assert!(count.0 >= 1, "at least one audit log entry should exist for the status change");
}
