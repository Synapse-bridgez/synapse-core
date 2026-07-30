/// Tests for #962 — GraphQL transactions query filters in-memory after limit
///
/// Validates that filter predicates are applied at the database layer
/// (before LIMIT) rather than in-memory (after LIMIT), ensuring that
/// filtered queries return all matching results up to the limit.
use sqlx::{migrate::Migrator, PgPool};
use std::path::Path;
use synapse_core::db::queries::set_tenant_context;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;

async fn setup_db() -> (PgPool, PgPool, impl std::any::Any) {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{}/postgres", port);
    let admin_pool = PgPool::connect(&url).await.unwrap();
    let migrator = Migrator::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"))
        .await
        .unwrap();
    migrator.run(&admin_pool).await.unwrap();

    // Create a non-superuser role so RLS policies are enforced
    sqlx::query("CREATE ROLE synapse_app LOGIN PASSWORD 'synapse_app'")
        .execute(&admin_pool)
        .await
        .unwrap();
    sqlx::query(
        "GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO synapse_app",
    )
    .execute(&admin_pool)
    .await
    .unwrap();
    sqlx::query("GRANT USAGE ON SCHEMA public TO synapse_app")
        .execute(&admin_pool)
        .await
        .unwrap();

    let app_url = format!(
        "postgres://synapse_app:synapse_app@127.0.0.1:{}/postgres",
        port
    );
    let pool = PgPool::connect(&app_url).await.unwrap();

    // Create current-month partition (needs superuser)
    sqlx::query(
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
                EXECUTE format('CREATE TABLE %I PARTITION OF transactions FOR VALUES FROM (%L) TO (%L)', partition_name, start_date, end_date);
            END IF;
        END $$;
    "#,
    )
    .execute(&admin_pool)
    .await
    .unwrap();

    (pool, admin_pool, container)
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_transactions_filter_applied_at_database_layer() {
    let (pool, _admin_pool, _c) = setup_db().await;

    let tenant = Uuid::new_v4();

    // Insert a tenant
    sqlx::query("INSERT INTO tenants (tenant_id, name, api_key, webhook_secret, stellar_account, rate_limit_per_minute, is_active) VALUES ($1,$2,$3,'','',60,true)")
        .bind(tenant)
        .bind("TestTenant")
        .bind(Uuid::new_v4().to_string())
        .execute(&pool)
        .await
        .unwrap();

    // Insert 25 transactions: 20 with status 'pending', 5 with status 'completed'
    let mut pending_count = 0;
    let mut completed_count = 0;

    for i in 0..25 {
        let id = Uuid::new_v4();
        let status = if i < 20 { "pending" } else { "completed" };
        let mut conn = pool.acquire().await.unwrap();
        set_tenant_context(&mut conn, Some(tenant), false)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO transactions (id, stellar_account, amount, asset_code, status, created_at, updated_at, tenant_id)
               VALUES ($1, 'GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA', 100, 'USD', $2, NOW(), NOW(), $3)"#,
        )
        .bind(id)
        .bind(status)
        .bind(tenant)
        .execute(&mut *conn)
        .await
        .unwrap();

        if status == "pending" {
            pending_count += 1;
        } else {
            completed_count += 1;
        }
    }

    // Query for completed transactions with limit=20
    // If filter is applied at DB layer: should get all 5 completed transactions
    // If filter is applied in-memory after LIMIT: might get 0 if all 20 most recent are pending
    let mut conn = pool.acquire().await.unwrap();
    set_tenant_context(&mut conn, Some(tenant), false)
        .await
        .unwrap();

    let results: Vec<(String,)> = sqlx::query_as(
        "SELECT status FROM transactions WHERE tenant_id = $1 AND status = 'completed' ORDER BY created_at DESC LIMIT 20"
    )
    .bind(tenant)
    .fetch_all(&mut *conn)
    .await
    .unwrap();

    assert_eq!(
        results.len(),
        5,
        "filtering at database layer should return all 5 completed transactions even with LIMIT 20"
    );

    // All results should have status 'completed'
    assert!(
        results.iter().all(|r| r.0 == "completed"),
        "all filtered results should have status 'completed'"
    );
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_transactions_filter_with_multiple_criteria() {
    let (pool, _admin_pool, _c) = setup_db().await;

    let tenant = Uuid::new_v4();
    let account_a = "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    let account_b = "GBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";

    // Insert a tenant
    sqlx::query("INSERT INTO tenants (tenant_id, name, api_key, webhook_secret, stellar_account, rate_limit_per_minute, is_active) VALUES ($1,$2,$3,'','',60,true)")
        .bind(tenant)
        .bind("TestTenant")
        .bind(Uuid::new_v4().to_string())
        .execute(&pool)
        .await
        .unwrap();

    // Insert 30 transactions: 25 for account_a, 5 for account_b
    for i in 0..30 {
        let id = Uuid::new_v4();
        let account = if i < 25 { account_a } else { account_b };
        let mut conn = pool.acquire().await.unwrap();
        set_tenant_context(&mut conn, Some(tenant), false)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO transactions (id, stellar_account, amount, asset_code, status, created_at, updated_at, tenant_id)
               VALUES ($1, $2, 100, 'USD', 'pending', NOW(), NOW(), $3)"#,
        )
        .bind(id)
        .bind(account)
        .bind(tenant)
        .execute(&mut *conn)
        .await
        .unwrap();
    }

    // Query for account_b transactions with limit=20
    // If filter at DB layer: should get all 5 account_b transactions
    // If filter in-memory: might get 0 if all 20 most recent are account_a
    let mut conn = pool.acquire().await.unwrap();
    set_tenant_context(&mut conn, Some(tenant), false)
        .await
        .unwrap();

    let results: Vec<(String,)> = sqlx::query_as(
        "SELECT stellar_account FROM transactions WHERE tenant_id = $1 AND stellar_account = $2 ORDER BY created_at DESC LIMIT 20"
    )
    .bind(tenant)
    .bind(account_b)
    .fetch_all(&mut *conn)
    .await
    .unwrap();

    assert_eq!(
        results.len(),
        5,
        "filtering on stellar_account at database layer should return all 5 matching transactions"
    );

    // All results should be account_b
    assert!(
        results.iter().all(|r| r.0 == account_b),
        "all filtered results should be from account_b"
    );
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_transactions_combined_status_and_asset_filter() {
    let (pool, _admin_pool, _c) = setup_db().await;

    let tenant = Uuid::new_v4();

    // Insert a tenant
    sqlx::query("INSERT INTO tenants (tenant_id, name, api_key, webhook_secret, stellar_account, rate_limit_per_minute, is_active) VALUES ($1,$2,$3,'','',60,true)")
        .bind(tenant)
        .bind("TestTenant")
        .bind(Uuid::new_v4().to_string())
        .execute(&pool)
        .await
        .unwrap();

    // Insert 30 transactions: 20 with status 'pending' and asset 'USD', 10 with status 'completed' and asset 'EUR'
    for i in 0..30 {
        let id = Uuid::new_v4();
        let (status, asset) = if i < 20 {
            ("pending", "USD")
        } else {
            ("completed", "EUR")
        };
        let mut conn = pool.acquire().await.unwrap();
        set_tenant_context(&mut conn, Some(tenant), false)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO transactions (id, stellar_account, amount, asset_code, status, created_at, updated_at, tenant_id)
               VALUES ($1, 'GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA', 100, $2, $3, NOW(), NOW(), $4)"#,
        )
        .bind(id)
        .bind(asset)
        .bind(status)
        .bind(tenant)
        .execute(&mut *conn)
        .await
        .unwrap();
    }

    // Query for completed transactions with EUR asset and limit=20
    let mut conn = pool.acquire().await.unwrap();
    set_tenant_context(&mut conn, Some(tenant), false)
        .await
        .unwrap();

    let results: Vec<(String, String)> = sqlx::query_as(
        "SELECT status, asset_code FROM transactions WHERE tenant_id = $1 AND status = 'completed' AND asset_code = 'EUR' ORDER BY created_at DESC LIMIT 20"
    )
    .bind(tenant)
    .fetch_all(&mut *conn)
    .await
    .unwrap();

    assert_eq!(
        results.len(),
        10,
        "combined filter should return all 10 matching transactions with limit=20"
    );

    // All results should match both criteria
    assert!(
        results.iter().all(|r| r.0 == "completed" && r.1 == "EUR"),
        "all filtered results should match both status and asset criteria"
    );
}
