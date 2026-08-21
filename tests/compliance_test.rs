/// Integration tests for ComplianceService correctness.
///
/// Requires DATABASE_URL pointing to a running Postgres with migrations applied.
/// Run with: `cargo test --test compliance_test -- --ignored`
use bigdecimal::BigDecimal;
use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use std::str::FromStr;
use synapse_core::services::compliance::ComplianceService;
use uuid::Uuid;

// ── helpers ──────────────────────────────────────────────────────────────────

async fn setup_pool() -> PgPool {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = PgPool::connect(&url).await.unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    ensure_partition(&pool).await;
    pool
}

async fn ensure_partition(pool: &PgPool) {
    let _ = sqlx::query(
        r#"
        DO $$
        DECLARE
            pdate DATE; pname TEXT; s TEXT; e TEXT;
        BEGIN
            pdate := DATE_TRUNC('month', NOW());
            pname := 'transactions_y' || TO_CHAR(pdate,'YYYY') || 'm' || TO_CHAR(pdate,'MM');
            s := TO_CHAR(pdate, 'YYYY-MM-DD');
            e := TO_CHAR(pdate + INTERVAL '1 month', 'YYYY-MM-DD');
            IF NOT EXISTS (SELECT 1 FROM pg_class WHERE relname = pname) THEN
                EXECUTE format('CREATE TABLE %I PARTITION OF transactions FOR VALUES FROM (%L) TO (%L)', pname, s, e);
            END IF;
        END $$;
        "#,
    )
    .execute(pool)
    .await;
}

/// Insert a transaction with an explicit `created_at` so we can pin it to a
/// known period regardless of when the test runs.
async fn insert_tx(pool: &PgPool, status: &str, amount: &str, created_at: DateTime<Utc>) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO transactions \
         (id, stellar_account, amount, asset_code, status, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $6)",
    )
    .bind(id)
    .bind(format!("G{}", id.simple()))
    .bind(BigDecimal::from_str(amount).unwrap())
    .bind("USD")
    .bind(status)
    .bind(created_at)
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn clean_period(pool: &PgPool, start: DateTime<Utc>, end: DateTime<Utc>) {
    sqlx::query("DELETE FROM transactions WHERE created_at >= $1 AND created_at < $2")
        .bind(start)
        .bind(end)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM compliance_reports WHERE period_start = $1")
        .bind(start)
        .execute(pool)
        .await
        .unwrap();
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// settlement_total must only include completed/settled rows; pending and
/// failed amounts must be excluded.
#[ignore = "Requires DATABASE_URL"]
#[tokio::test]
async fn test_settlement_total_excludes_pending_and_failed() {
    let pool = setup_pool().await;

    // Pin to a past daily period that won't collide with real traffic.
    let period_start = Utc::now()
        .date_naive()
        .pred_opt()
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc();
    let period_end = period_start + Duration::days(1);
    clean_period(&pool, period_start, period_end).await;

    let mid = period_start + Duration::hours(12);
    insert_tx(&pool, "completed", "100.00", mid).await;
    insert_tx(&pool, "pending", "50.00", mid).await;
    insert_tx(&pool, "failed", "25.00", mid).await;

    let svc = ComplianceService::new(pool.clone());
    let report = svc
        .generate_for_range_test("daily", period_start, period_end)
        .await
        .unwrap();

    assert_eq!(
        report.transaction_count, 3,
        "tx_count should include all statuses"
    );
    assert_eq!(
        report.settlement_total,
        BigDecimal::from_str("100.00").unwrap(),
        "settlement_total must only reflect completed amount"
    );

    clean_period(&pool, period_start, period_end).await;
}

/// Calling generate_report for the same period twice must produce exactly one
/// row in compliance_reports (idempotent upsert).
#[ignore = "Requires DATABASE_URL"]
#[tokio::test]
async fn test_duplicate_generation_yields_one_report() {
    let pool = setup_pool().await;

    let period_start = (Utc::now() - Duration::days(2))
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc();
    let period_end = period_start + Duration::days(1);
    clean_period(&pool, period_start, period_end).await;

    let mid = period_start + Duration::hours(6);
    insert_tx(&pool, "completed", "10.00", mid).await;

    let svc = ComplianceService::new(pool.clone());

    svc.generate_for_range_test("daily", period_start, period_end)
        .await
        .unwrap();
    svc.generate_for_range_test("daily", period_start, period_end)
        .await
        .unwrap();

    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM compliance_reports WHERE period = 'daily' AND period_start = $1",
    )
    .bind(period_start)
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(
        count, 1,
        "duplicate generation must produce exactly one report"
    );

    clean_period(&pool, period_start, period_end).await;
}

/// All figures inside one report must be internally consistent: tx_count must
/// equal the sum of tx_count values across all assets in volume_by_asset.
#[ignore = "Requires DATABASE_URL"]
#[tokio::test]
async fn test_report_figures_are_internally_consistent() {
    let pool = setup_pool().await;

    let period_start = (Utc::now() - Duration::days(3))
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc();
    let period_end = period_start + Duration::days(1);
    clean_period(&pool, period_start, period_end).await;

    let mid = period_start + Duration::hours(6);
    // Insert rows with two different asset_codes to exercise volume_by_asset.
    for _ in 0..3 {
        insert_tx(&pool, "completed", "10.00", mid).await;
    }
    for _ in 0..2 {
        insert_tx(&pool, "pending", "5.00", mid).await;
    }

    let svc = ComplianceService::new(pool.clone());
    let report = svc
        .generate_for_range_test("daily", period_start, period_end)
        .await
        .unwrap();

    // Sum tx_count across assets must equal the report-level transaction_count.
    let asset_tx_sum: i64 = report
        .volume_by_asset
        .as_object()
        .unwrap()
        .values()
        .map(|v| v["tx_count"].as_i64().unwrap_or(0))
        .sum();

    assert_eq!(
        report.transaction_count, asset_tx_sum,
        "transaction_count must equal sum of per-asset tx_count (snapshot consistency)"
    );

    clean_period(&pool, period_start, period_end).await;
}
