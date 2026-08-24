/// Regression test for the empirical re-audit finding in the tracked issue:
/// a correctly-written RLS policy (migrations/20260501000000_tenant_rls.sql)
/// provides zero protection when the connected role has `rolbypassrls =
/// true` — which is exactly the case for the `initdb` bootstrap superuser
/// role (whatever `POSTGRES_USER` names it), the role every
/// docker-compose.yml/docker-compose.dev.yml/CI configuration in this repo
/// connected the app itself as before this fix.
///
/// (Verified directly against a real Postgres instance while writing this
/// test: `rolbypassrls` is NOT inherited by roles a superuser subsequently
/// creates — a freshly `CREATE ROLE`'d role defaults to `rolbypassrls =
/// false` regardless of who created it. It's specifically the bootstrap
/// role itself that has it set. See the first test below for the exact
/// query that confirmed this.)
///
/// This test proves both halves directly against a real Postgres instance:
/// - the bootstrap superuser role (what every old config connected the app
///   as) leaks both tenants' rows despite RLS being enabled and forced;
/// - the role this fix actually ships (see
///   scripts/db-init/01-create-app-role.sql — LOGIN, explicit NOBYPASSRLS)
///   does not.
use sqlx::{migrate::Migrator, PgPool};
use std::path::Path;
use synapse_core::db::queries;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;

async fn setup(container_url: &str) -> PgPool {
    let admin_pool = PgPool::connect(container_url).await.unwrap();
    let migrator = Migrator::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"))
        .await
        .unwrap();
    migrator.run(&admin_pool).await.unwrap();
    admin_pool
}

async fn create_current_month_partition(admin_pool: &PgPool) {
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
    .execute(admin_pool)
    .await
    .unwrap();
}

async fn insert_tenant_with_transaction(admin_pool: &PgPool, name: &str) -> (Uuid, Uuid) {
    let tenant_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO tenants (tenant_id, name, api_key, webhook_secret, stellar_account, rate_limit_per_minute, is_active) \
         VALUES ($1,$2,$3,'','',60,true)",
    )
    .bind(tenant_id)
    .bind(name)
    .bind(Uuid::new_v4().to_string())
    .execute(admin_pool)
    .await
    .unwrap();

    let tx_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO transactions (id, stellar_account, amount, asset_code, status, created_at, updated_at, tenant_id)
           VALUES ($1, 'GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA', 100, 'USD', 'pending', NOW(), NOW(), $2)"#,
    )
    .bind(tx_id)
    .bind(tenant_id)
    .execute(admin_pool)
    .await
    .unwrap();

    (tenant_id, tx_id)
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn bootstrap_superuser_role_leaks_across_tenants() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let admin_pool = setup(&url).await;
    create_current_month_partition(&admin_pool).await;

    let (_tenant_a, tx_a) = insert_tenant_with_transaction(&admin_pool, "TenantA").await;
    let (_tenant_b, tx_b) = insert_tenant_with_transaction(&admin_pool, "TenantB").await;

    // NOTE: this test originally created a *separate* role with no explicit
    // BYPASSRLS clause, expecting Postgres to default that to true. It
    // doesn't — BYPASSRLS is never inherited from the creating role and
    // defaults to false for any newly created role, including ones a
    // superuser creates. Verified directly against a real instance while
    // writing this test:
    //
    //   SELECT rolname, rolsuper, rolbypassrls FROM pg_roles
    //     WHERE rolname IN ('postgres', 'freshly_created_role');
    //   postgres              | t | t   <- the initdb bootstrap superuser
    //   freshly_created_role  | f | f   <- superuser-created, but NOT bypassing
    //
    // The real defect this fix closes is that every docker-compose.yml/CI
    // config in this repo connected the *app itself* as `synapse` — the
    // initdb bootstrap role, i.e. exactly the `postgres` role here — not as
    // some other role that superuser separately created. `admin_pool`
    // (connected as `postgres`) already *is* that scenario, so this test
    // uses it directly rather than manufacturing a redundant second role.
    let bypasses: bool =
        sqlx::query_scalar("SELECT rolbypassrls FROM pg_roles WHERE rolname = current_user")
            .fetch_one(&admin_pool)
            .await
            .unwrap();
    assert!(
        bypasses,
        "sanity check: the initdb bootstrap superuser role must have rolbypassrls = true — \
         this is the exact condition this fix closes (see assert_no_bypassrls in startup.rs, \
         which checks exactly this column against the app's own connection)"
    );

    // No tenant context set at all — this is the live query layer, called
    // exactly the way every unauthenticated request to GET /transactions
    // called it before this fix, over the same connection every
    // docker-compose.yml/CI config in this repo used for the app itself.
    let rows = queries::list_transactions(&admin_pool, 100, None, false)
        .await
        .unwrap();

    // Transaction (src/db/models.rs) has no tenant_id field at all — `SELECT
    // *` returns the column but sqlx's FromRow silently drops columns with
    // no matching struct field, so tenant identity isn't visible on the
    // decoded row. Checking by transaction id is an equally direct proof
    // that both tenants' *rows* came back, which is what matters here.
    let ids: std::collections::HashSet<Uuid> = rows.iter().map(|t| t.id).collect();
    assert!(
        ids.contains(&tx_a) && ids.contains(&tx_b),
        "both tenants' transactions came back through the bootstrap superuser role with no \
         tenant context set — RLS provided zero protection, matching the issue's empirical \
         finding"
    );

    let fetched = queries::get_transaction(&admin_pool, tx_b).await;
    assert!(
        fetched.is_ok(),
        "get_transaction on the bootstrap superuser role must not fail closed — it returns \
         tenant B's row to a connection with no tenant context, proving the leak at the \
         single-row lookup used by GET /transactions/:id too"
    );
}

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn nobypassrls_role_with_no_tenant_context_sees_nothing() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let admin_pool = setup(&url).await;
    create_current_month_partition(&admin_pool).await;

    insert_tenant_with_transaction(&admin_pool, "TenantA").await;
    insert_tenant_with_transaction(&admin_pool, "TenantB").await;

    // The role this fix actually ships — see scripts/db-init/01-create-app-role.sql.
    sqlx::query(
        "CREATE ROLE fixed_role LOGIN PASSWORD 'fixed_role' NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS",
    )
    .execute(&admin_pool)
    .await
    .unwrap();
    sqlx::query(
        "GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO fixed_role",
    )
    .execute(&admin_pool)
    .await
    .unwrap();
    sqlx::query("GRANT USAGE ON SCHEMA public TO fixed_role")
        .execute(&admin_pool)
        .await
        .unwrap();

    let bypasses: bool =
        sqlx::query_scalar("SELECT rolbypassrls FROM pg_roles WHERE rolname = 'fixed_role'")
            .fetch_one(&admin_pool)
            .await
            .unwrap();
    assert!(!bypasses, "fixed_role must not bypass RLS");

    let fixed_pool = PgPool::connect(&format!(
        "postgres://fixed_role:fixed_role@127.0.0.1:{port}/postgres"
    ))
    .await
    .unwrap();

    // Session-level default in this codebase is app.is_admin = true (see
    // db::set_session_admin_context) — but that's set by *this app's* pool
    // construction, not by Postgres itself. A bare connection like this one,
    // exactly mirroring what a manual/ad-hoc query against this role would
    // see, has no RLS context set at all and must fail closed to nothing
    // rather than leak every tenant's rows.
    let rows = queries::list_transactions(&fixed_pool, 100, None, false)
        .await
        .unwrap();
    assert!(
        rows.is_empty(),
        "a NOBYPASSRLS role with no app.tenant_id/app.is_admin set must see zero rows \
         (fail closed), not every tenant's data"
    );
}
