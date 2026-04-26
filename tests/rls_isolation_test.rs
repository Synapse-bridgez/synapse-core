/// Tests for #270 — Row-Level Security for Multi-Tenant Data Isolation
///
/// Validates:
/// - Tenant A cannot see tenant B's transactions
/// - Admin (is_admin=true) can see all transactions
/// - Existing single-tenant queries work (tenant_id defaults to NULL)
use sqlx::{migrate::Migrator, PgPool};
use std::path::Path;
use synapse_core::db::queries::set_tenant_context;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;

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

/// Insert a transaction row with an explicit tenant_id.
async fn insert_tx_for_tenant(pool: &PgPool, tenant_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(r#"
        INSERT INTO transactions (id, stellar_account, amount, asset_code, status, created_at, updated_at, tenant_id)
        VALUES ($1, 'GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA', 100, 'USD', 'pending', NOW(), NOW(), $2)
    "#)
    .bind(id)
    .bind(tenant_id)
    .execute(pool)
    .await
    .unwrap();
    id
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_tenant_a_cannot_see_tenant_b_transactions() {
    let (pool, _c) = setup_db().await;

    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();

    // Insert tenants (required for FK)
    for (tid, name) in [(tenant_a, "TenantA"), (tenant_b, "TenantB")] {
        sqlx::query("INSERT INTO tenants (tenant_id, name, webhook_secret, stellar_account, rate_limit_per_minute, is_active) VALUES ($1,$2,'','',60,true)")
            .bind(tid).bind(name).execute(&pool).await.unwrap();
    }

    let tx_b = insert_tx_for_tenant(&pool, tenant_b).await;

    // Query as tenant A — should not see tenant B's transaction
    let mut conn = pool.acquire().await.unwrap();
    set_tenant_context(&mut conn, Some(tenant_a), false).await.unwrap();

    let row: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM transactions WHERE id = $1")
        .bind(tx_b)
        .fetch_optional(&mut *conn)
        .await
        .unwrap();

    assert!(row.is_none(), "tenant A should not see tenant B's transaction via RLS");
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_admin_can_see_all_transactions() {
    let (pool, _c) = setup_db().await;

    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();

    for (tid, name) in [(tenant_a, "AdminTA"), (tenant_b, "AdminTB")] {
        sqlx::query("INSERT INTO tenants (tenant_id, name, webhook_secret, stellar_account, rate_limit_per_minute, is_active) VALUES ($1,$2,'','',60,true)")
            .bind(tid).bind(name).execute(&pool).await.unwrap();
    }

    let tx_a = insert_tx_for_tenant(&pool, tenant_a).await;
    let tx_b = insert_tx_for_tenant(&pool, tenant_b).await;

    // Query as admin — should see both
    let mut conn = pool.acquire().await.unwrap();
    set_tenant_context(&mut conn, None, true).await.unwrap();

    let rows: Vec<(Uuid,)> = sqlx::query_as("SELECT id FROM transactions WHERE id = ANY($1)")
        .bind(vec![tx_a, tx_b])
        .fetch_all(&mut *conn)
        .await
        .unwrap();

    assert_eq!(rows.len(), 2, "admin should see transactions from all tenants");
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_null_tenant_id_rows_visible_to_admin() {
    let (pool, _c) = setup_db().await;

    // Insert a legacy row with no tenant_id
    let id = Uuid::new_v4();
    sqlx::query(r#"
        INSERT INTO transactions (id, stellar_account, amount, asset_code, status, created_at, updated_at)
        VALUES ($1, 'GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA', 50, 'USD', 'pending', NOW(), NOW())
    "#)
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();

    let mut conn = pool.acquire().await.unwrap();
    set_tenant_context(&mut conn, None, true).await.unwrap();

    let row: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM transactions WHERE id = $1")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await
        .unwrap();

    assert!(row.is_some(), "admin should see legacy rows with NULL tenant_id");
}
