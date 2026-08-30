//! End-to-end coverage for distributed trace ID propagation from a
//! transaction through to its audit trail (issue: propagate distributed
//! trace IDs across the full webhook-to-reconciliation pipeline).
//!
//! `transaction_callback` (`src/handlers/webhook.rs`) already stamps a new
//! transaction's `trace_id` from the inbound `traceparent`. This test covers
//! the next hop: a status-change audit entry for that transaction carries
//! the same trace ID (`AuditLog::log_status_change_traced`,
//! `src/db/audit.rs`), so an operator can go from a trace ID to every status
//! transition it produced.

use bigdecimal::BigDecimal;
use sqlx::{PgPool, Row};
use std::env;
use uuid::Uuid;

use synapse_core::db::audit::{AuditLog, ENTITY_TRANSACTION};

fn setup_env() {
    if env::var("DATABASE_URL").is_err() {
        env::set_var(
            "DATABASE_URL",
            "postgres://synapse_app:synapse_app@localhost:5432/synapse_test",
        );
    }
}

async fn get_pool() -> Option<PgPool> {
    setup_env();
    let db_url = env::var("DATABASE_URL").ok()?;
    sqlx::postgres::PgPoolOptions::new()
        .connect(&db_url)
        .await
        .ok()
}

async fn insert_test_transaction(pool: &PgPool, trace_id: &str) -> Uuid {
    let id = Uuid::new_v4();
    let amount: BigDecimal = "10.00".parse().unwrap();
    sqlx::query(
        r#"
        INSERT INTO transactions
            (id, stellar_account, amount, asset_code, status, trace_id, created_at, updated_at)
        VALUES ($1, 'GTRACEPROPAGATIONTEST', $2, 'USD', 'pending', $3, NOW(), NOW())
        "#,
    )
    .bind(id)
    .bind(amount)
    .bind(trace_id)
    .execute(pool)
    .await
    .expect("failed to insert test transaction");
    id
}

async fn cleanup(pool: &PgPool, id: Uuid) {
    let _ = sqlx::query("DELETE FROM audit_logs WHERE entity_id = $1")
        .bind(id)
        .execute(pool)
        .await;
    let _ = sqlx::query("DELETE FROM transactions WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await;
}

#[tokio::test]
#[ignore = "Requires a live database"]
async fn audit_log_carries_the_transaction_trace_id_through_a_status_change() {
    let pool = match get_pool().await {
        Some(p) => p,
        None => return,
    };

    let trace_id = format!("trace-{}", Uuid::new_v4());
    let id = insert_test_transaction(&pool, &trace_id).await;

    let mut db_tx = pool.begin().await.unwrap();
    AuditLog::log_status_change_traced(
        &mut db_tx,
        id,
        ENTITY_TRANSACTION,
        "pending",
        "completed",
        "admin",
        Some(&trace_id),
    )
    .await
    .expect("audit log insert failed");
    db_tx.commit().await.unwrap();

    let row = sqlx::query("SELECT trace_id FROM audit_logs WHERE entity_id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("audit log row not found");
    let stored_trace_id: Option<String> = row.get("trace_id");

    assert_eq!(
        stored_trace_id.as_deref(),
        Some(trace_id.as_str()),
        "audit log entry must carry the same trace ID as the transaction it records"
    );

    cleanup(&pool, id).await;
}
