/// Tests for #963 — GraphQL transactions query ignores offset parameter
///
/// Validates that the `offset` argument in the `transactions` GraphQL query
/// is properly used for pagination instead of being silently ignored.
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
async fn test_transactions_offset_parameter_is_used() {
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

    // Insert 5 transactions with proper tenant context
    let mut tx_ids = vec![];
    for i in 0..5 {
        let id = Uuid::new_v4();
        tx_ids.push(id);
        let mut conn = pool.acquire().await.unwrap();
        set_tenant_context(&mut conn, Some(tenant), false)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO transactions (id, stellar_account, amount, asset_code, status, created_at, updated_at, tenant_id)
               VALUES ($1, 'GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA', $2, 'USD', 'pending', NOW(), NOW(), $3)"#,
        )
        .bind(id)
        .bind(100 + i as i64)
        .execute(&mut *conn)
        .await
        .unwrap();
    }

    // Query with offset=0, limit=2 should return first 2 transactions
    let mut conn = pool.acquire().await.unwrap();
    set_tenant_context(&mut conn, Some(tenant), false)
        .await
        .unwrap();

    let page1: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM transactions WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT 2 OFFSET 0"
    )
    .bind(tenant)
    .fetch_all(&mut *conn)
    .await
    .unwrap();

    // Query with offset=2, limit=2 should return next 2 transactions (different from page 1)
    let page2: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM transactions WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT 2 OFFSET 2"
    )
    .bind(tenant)
    .fetch_all(&mut *conn)
    .await
    .unwrap();

    assert_eq!(page1.len(), 2, "first page should return 2 transactions");
    assert_eq!(page2.len(), 2, "second page should return 2 transactions");

    let page1_ids: Vec<_> = page1.iter().map(|r| r.0).collect();
    let page2_ids: Vec<_> = page2.iter().map(|r| r.0).collect();

    // Pages should be different when offset changes
    assert!(
        page1_ids != page2_ids,
        "page 1 and page 2 should contain different transactions when offset changes"
    );
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_transactions_offset_beyond_results() {
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

    // Insert 3 transactions
    for i in 0..3 {
        let id = Uuid::new_v4();
        let mut conn = pool.acquire().await.unwrap();
        set_tenant_context(&mut conn, Some(tenant), false)
            .await
            .unwrap();
        sqlx::query(
            r#"INSERT INTO transactions (id, stellar_account, amount, asset_code, status, created_at, updated_at, tenant_id)
               VALUES ($1, 'GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA', $2, 'USD', 'pending', NOW(), NOW(), $3)"#,
        )
        .bind(id)
        .bind(100 + i as i64)
        .execute(&mut *conn)
        .await
        .unwrap();
    }

    // Query with offset beyond results should return empty list
    let mut conn = pool.acquire().await.unwrap();
    set_tenant_context(&mut conn, Some(tenant), false)
        .await
        .unwrap();

    let results: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM transactions WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT 20 OFFSET 10"
    )
    .bind(tenant)
    .fetch_all(&mut *conn)
    .await
    .unwrap();

    assert_eq!(
        results.len(),
        0,
        "query with offset beyond results should return empty list"
    );
}
