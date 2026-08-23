//! Part C regression test: two concurrent reconciliation runs for the same
//! window (e.g. two unguarded scheduler instances, or leader election
//! failing open on both at once) must produce exactly one stored report,
//! not two.

use chrono::Utc;
use sqlx::{migrate::Migrator, PgPool, Row};
use std::path::Path;
use synapse_core::services::reconciliation::{ReconciliationReport, ReconciliationService};
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

fn report_for_period(
    period_start: chrono::DateTime<Utc>,
    period_end: chrono::DateTime<Utc>,
) -> ReconciliationReport {
    ReconciliationReport {
        generated_at: Utc::now(),
        period_start,
        period_end,
        total_db_transactions: 10,
        total_chain_payments: 10,
        missing_on_chain: vec![],
        orphaned_payments: vec![],
        amount_mismatches: vec![],
    }
}

#[ignore = "Requires Docker"]
#[tokio::test]
async fn test_concurrent_reconciliation_runs_produce_exactly_one_report() {
    let (pool, _container) = setup_test_db().await;

    let period_end = Utc::now();
    let period_start = period_end - chrono::Duration::days(1);

    // Two "instances" independently compute a report for the identical
    // window and race to store it — this is exactly what two unguarded
    // ReconciliationJob executions produce.
    let report_a = report_for_period(period_start, period_end);
    let report_b = report_for_period(period_start, period_end);

    let (result_a, result_b) = tokio::join!(
        ReconciliationService::store_report(&pool, &report_a),
        ReconciliationService::store_report(&pool, &report_b),
    );

    let inserted_a = result_a.unwrap();
    let inserted_b = result_b.unwrap();
    assert!(
        inserted_a ^ inserted_b,
        "expected exactly one of the two concurrent inserts to win, got a={inserted_a} b={inserted_b}"
    );

    let row = sqlx::query(
        "SELECT COUNT(*) as cnt FROM reconciliation_reports WHERE period_start = $1 AND period_end = $2",
    )
    .bind(period_start)
    .bind(period_end)
    .fetch_one(&pool)
    .await
    .unwrap();
    let count: i64 = row.get("cnt");
    assert_eq!(
        count, 1,
        "expected exactly one report row for the period, not a duplicate"
    );
}
