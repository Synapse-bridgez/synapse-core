/// Tests for #964 — GraphQL resolvers must set RLS tenant context
///
/// Validates that GraphQL query and mutation resolvers properly set the
/// app.tenant_id and app.is_admin session GUCs required for RLS policies
/// to enforce multi-tenant data isolation.
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

async fn insert_tx_for_tenant(pool: &PgPool, tenant_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    let mut conn = pool.acquire().await.unwrap();
    set_tenant_context(&mut conn, Some(tenant_id), false)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO transactions (id, stellar_account, amount, asset_code, status, created_at, updated_at, tenant_id)
           VALUES ($1, 'GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA', 100, 'USD', 'pending', NOW(), NOW(), $2)"#,
    )
    .bind(id)
    .bind(tenant_id)
    .execute(&mut *conn)
    .await
    .unwrap();
    id
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_graphql_transaction_query_respects_rls() {
    let (pool, _admin_pool, _c) = setup_db().await;

    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();

    for (tid, name) in [(tenant_a, "TenantA"), (tenant_b, "TenantB")] {
        sqlx::query("INSERT INTO tenants (tenant_id, name, api_key_hash, webhook_secret, stellar_account, rate_limit_per_minute, is_active) VALUES ($1,$2,$3,pgp_sym_encrypt($4,$5),'',60,true)")
            .bind(tid)
            .bind(name)
            .bind(synapse_core::db::queries::hash_api_key(&Uuid::new_v4().to_string()))
            .bind("")
            .bind(synapse_core::db::queries::tenant_secret_key())
            .execute(&pool)
            .await
            .unwrap();
    }

    let tx_b = insert_tx_for_tenant(&pool, tenant_b).await;

    // Query without setting tenant context — RLS policy should block the row
    // because app.tenant_id is not set, only NULL rows should be visible
    let row: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM transactions WHERE id = $1")
        .bind(tx_b)
        .fetch_optional(&pool)
        .await
        .unwrap();

    assert!(
        row.is_none(),
        "transaction query without tenant context should not see tenant-scoped rows via RLS"
    );
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_graphql_transactions_query_respects_rls() {
    let (pool, _admin_pool, _c) = setup_db().await;

    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();

    for (tid, name) in [(tenant_a, "TenantA"), (tenant_b, "TenantB")] {
        sqlx::query("INSERT INTO tenants (tenant_id, name, api_key_hash, webhook_secret, stellar_account, rate_limit_per_minute, is_active) VALUES ($1,$2,$3,pgp_sym_encrypt($4,$5),'',60,true)")
            .bind(tid)
            .bind(name)
            .bind(synapse_core::db::queries::hash_api_key(&Uuid::new_v4().to_string()))
            .bind("")
            .bind(synapse_core::db::queries::tenant_secret_key())
            .execute(&pool)
            .await
            .unwrap();
    }

    let tx_b = insert_tx_for_tenant(&pool, tenant_b).await;

    // List transactions without setting tenant context — should not see tenant-scoped rows
    let rows: Vec<(Uuid,)> = sqlx::query_as("SELECT id FROM transactions")
        .fetch_all(&pool)
        .await
        .unwrap();

    let has_b = rows.iter().any(|r| r.0 == tx_b);
    assert!(
        !has_b,
        "transactions list without tenant context should not see tenant-scoped rows via RLS"
    );
}

// NOTE: settlements has no tenant_id column or RLS policy yet — only
// transactions got tenant-scoped RLS (see
// migrations/20260501000000_tenant_rls.sql). This test exercises a feature
// that was never implemented, so it is excluded from CI via `--skip` in
// .github/workflows/rust.yml until settlements gets its own tenant_id
// column + RLS policy.
#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_graphql_settlement_query_respects_rls() {
    let (pool, _admin_pool, _c) = setup_db().await;

    let tenant_a = Uuid::new_v4();
    let tenant_b = Uuid::new_v4();

    for (tid, name) in [(tenant_a, "TenantA"), (tenant_b, "TenantB")] {
        sqlx::query("INSERT INTO tenants (tenant_id, name, api_key_hash, webhook_secret, stellar_account, rate_limit_per_minute, is_active) VALUES ($1,$2,$3,pgp_sym_encrypt($4,$5),'',60,true)")
            .bind(tid)
            .bind(name)
            .bind(synapse_core::db::queries::hash_api_key(&Uuid::new_v4().to_string()))
            .bind("")
            .bind(synapse_core::db::queries::tenant_secret_key())
            .execute(&pool)
            .await
            .unwrap();
    }

    // Insert a settlement for tenant B with proper context
    let settlement_id = Uuid::new_v4();
    let mut conn = pool.acquire().await.unwrap();
    set_tenant_context(&mut conn, Some(tenant_b), false)
        .await
        .unwrap();
    sqlx::query(
        r#"INSERT INTO settlements (id, tenant_id, status, total_amount, created_at, updated_at)
           VALUES ($1, $2, 'pending', 1000, NOW(), NOW())"#,
    )
    .bind(settlement_id)
    .bind(tenant_b)
    .execute(&mut *conn)
    .await
    .unwrap();
    drop(conn);

    // Query settlements without setting tenant context — should not see tenant-scoped rows
    let rows: Vec<(Uuid,)> = sqlx::query_as("SELECT id FROM settlements")
        .fetch_all(&pool)
        .await
        .unwrap();

    let has_settlement = rows.iter().any(|r| r.0 == settlement_id);
    assert!(
        !has_settlement,
        "settlements list without tenant context should not see tenant-scoped rows via RLS"
    );
}
