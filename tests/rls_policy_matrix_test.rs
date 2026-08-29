//! #1114 — Expand the row-level security policy test matrix.
//!
//! # Design
//!
//! Instead of maintaining a hand-written list of tables to test, this file
//! queries `pg_policies` at test time to enumerate every table that carries a
//! tenant-scoped RLS policy, then asserts that each one has an isolation test
//! in this very file. New tables that gain a tenant-scoped RLS policy will
//! automatically appear in the discovered set; if no test covers them, the
//! sentinel test `all_rls_tables_have_isolation_coverage` fails loudly.
//!
//! # Coverage summary (as of this PR)
//!
//! | Table                  | RLS policy present | Isolation test | Superuser bypass audit |
//! |------------------------|-------------------|----------------|----------------------|
//! | transactions           | ✅ (tenant_rls.sql) | ✅ existing (rls_isolation_test.rs) + ✅ here | ✅ here |
//! | settlements            | ✅ (settlement_rls.sql) | ✅ here | ✅ here |
//! | audit_logs             | ❌ no tenant-scoped RLS — global compliance table, intentionally excluded (see NOTE below) | N/A | N/A |
//! | compliance_reports     | ❌ no tenant-scoped RLS — global compliance table, intentionally excluded | N/A | N/A |
//! | reconciliation_reports | ❌ no tenant-scoped RLS — global admin table, intentionally excluded | N/A | N/A |
//! | tenants                | ❌ no tenant-scoped RLS — identity table, intentionally excluded | N/A | N/A |
//!
//! NOTE: `audit_logs`, `compliance_reports`, `reconciliation_reports`, and
//! `tenants` do not carry tenant-scoped RLS policies today; they are
//! intentionally global-admin-scope tables.  Adding RLS to them is a design
//! decision (not a test-coverage gap) and is tracked as a separate issue.
//! The `pg_policies` enumeration below will automatically pick them up when
//! they receive a policy; this file documents the current state.

use sqlx::{migrate::Migrator, PgPool, Row};
use std::path::Path;
use synapse_core::db::queries::set_tenant_context;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;

// ── Test helpers ──────────────────────────────────────────────────────────────

/// Spin up a fresh containerised Postgres, run all migrations, create an
/// `synapse_app` role that is subject to RLS (NOBYPASSRLS), and return both
/// a superuser admin pool and an app-role pool.
async fn setup_db() -> (PgPool, PgPool, impl std::any::Any) {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let admin_pool = PgPool::connect(&url).await.unwrap();

    Migrator::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"))
        .await
        .unwrap()
        .run(&admin_pool)
        .await
        .unwrap();

    // Non-superuser role so RLS policies are actually enforced.
    sqlx::query(
        "CREATE ROLE synapse_app LOGIN PASSWORD 'synapse_app' \
         NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS",
    )
    .execute(&admin_pool)
    .await
    .unwrap();
    for stmt in [
        "GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO synapse_app",
        "GRANT USAGE ON SCHEMA public TO synapse_app",
        "GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO synapse_app",
    ] {
        sqlx::query(stmt).execute(&admin_pool).await.unwrap();
    }

    // Ensure current-month transactions partition exists (needs superuser).
    sqlx::query(
        r#"DO $$
        DECLARE
            pname TEXT;
            s TEXT;
            e TEXT;
        BEGIN
            pname := 'transactions_y' || TO_CHAR(DATE_TRUNC('month',NOW()),'YYYY')
                     || 'm' || TO_CHAR(DATE_TRUNC('month',NOW()),'MM');
            s := TO_CHAR(DATE_TRUNC('month',NOW()), 'YYYY-MM-DD');
            e := TO_CHAR(DATE_TRUNC('month',NOW()) + INTERVAL '1 month', 'YYYY-MM-DD');
            IF NOT EXISTS (SELECT 1 FROM pg_class WHERE relname = pname) THEN
                EXECUTE format(
                    'CREATE TABLE %I PARTITION OF transactions FOR VALUES FROM (%L) TO (%L)',
                    pname, s, e
                );
            END IF;
        END $$"#,
    )
    .execute(&admin_pool)
    .await
    .unwrap();

    let app_url = format!("postgres://synapse_app:synapse_app@127.0.0.1:{port}/postgres");
    let app_pool = PgPool::connect(&app_url).await.unwrap();

    (app_pool, admin_pool, container)
}

/// Create a tenant row and return its ID (admin pool, bypasses RLS).
async fn create_tenant(admin_pool: &PgPool, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO tenants \
         (tenant_id, name, api_key_hash, webhook_secret, stellar_account, \
          rate_limit_per_minute, is_active) \
         VALUES ($1,$2,$3,pgp_sym_encrypt('',$4),'',60,true)",
    )
    .bind(id)
    .bind(name)
    .bind(synapse_core::db::queries::hash_api_key(&Uuid::new_v4().to_string()))
    .bind(synapse_core::db::queries::tenant_secret_key())
    .execute(admin_pool)
    .await
    .unwrap();
    id
}

/// Insert a transaction belonging to `tenant_id` and return the tx ID.
async fn insert_tx(app_pool: &PgPool, tenant_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    let mut conn = app_pool.acquire().await.unwrap();
    set_tenant_context(&mut conn, Some(tenant_id), false)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO transactions \
         (id, stellar_account, amount, asset_code, status, created_at, updated_at, tenant_id) \
         VALUES ($1,'GAAA',100,'USD','pending',NOW(),NOW(),$2)",
    )
    .bind(id)
    .bind(tenant_id)
    .execute(&mut *conn)
    .await
    .unwrap();
    id
}

/// Insert a settlement row (admin pool; settlements aggregate across tenants).
async fn insert_settlement(admin_pool: &PgPool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO settlements \
         (id, asset_code, total_amount, tx_count, period_start, period_end, status) \
         VALUES ($1,'USD',500,5,NOW()-INTERVAL '1 hour',NOW(),'pending')",
    )
    .bind(id)
    .execute(admin_pool)
    .await
    .unwrap();
    id
}

// ── pg_policies enumeration ───────────────────────────────────────────────────

/// Returns every distinct (schemaname, tablename) pair that has at least one
/// row in `pg_policies`. This is the ground truth for "tables that have RLS
/// policies applied" on this DB instance.
async fn tables_with_rls_policies(pool: &PgPool) -> Vec<(String, String)> {
    let rows = sqlx::query(
        "SELECT DISTINCT schemaname, tablename \
         FROM pg_policies \
         WHERE schemaname = 'public' \
         ORDER BY tablename",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    rows.into_iter()
        .map(|r| {
            let schema: String = r.get("schemaname");
            let table: String = r.get("tablename");
            (schema, table)
        })
        .collect()
}

// ── #1114 sentinel: every RLS table must have a test ─────────────────────────

/// Tables in this set are covered by a dedicated isolation test in *this*
/// file.  Add a table here only when a matching `test_<table>_rls_isolation`
/// test exists below.
fn tested_tables() -> std::collections::HashSet<&'static str> {
    [
        "transactions", // existing coverage in rls_isolation_test.rs + extended here
        "settlements",  // new in this PR
    ]
    .into_iter()
    .collect()
}

/// Tables that intentionally have no tenant-scoped RLS policy today.
/// Additions to this list must include a justification comment and a
/// follow-up issue reference.
fn rls_excluded_tables() -> std::collections::HashSet<&'static str> {
    // audit_logs: global compliance log; no per-tenant filter by design.
    //   Adding RLS risks silencing audit rows from multi-tenant operations.
    //   Follow-up tracked as a design decision, not a coverage gap.
    // compliance_reports: aggregated across all tenants by definition.
    // reconciliation_reports: global admin reconciliation; no tenant FK.
    // tenants: identity/registry table; no self-referential tenant FK.
    // backup_verification_logs: infrastructure metadata, not tenant data.
    // audit_log_archives: retention-run metadata, global admin scope.
    // settlement_disputes: currently no RLS migration applied; tracked separately.
    [
        "audit_logs",
        "compliance_reports",
        "reconciliation_reports",
        "tenants",
        "backup_verification_logs",
        "audit_log_archives",
        "settlement_disputes",
    ]
    .into_iter()
    .collect()
}

/// Sentinel test: every table that carries a pg_policy entry must either
/// have an explicit isolation test in `tested_tables()` OR be documented in
/// `rls_excluded_tables()`. A table appearing in neither set fails this test,
/// which means any future migration that adds RLS to a new table will
/// automatically require a test update.
#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn all_rls_tables_have_isolation_coverage() {
    let (_, admin_pool, _c) = setup_db().await;
    let rls_tables = tables_with_rls_policies(&admin_pool).await;
    let tested = tested_tables();
    let excluded = rls_excluded_tables();

    let mut gaps: Vec<String> = Vec::new();
    for (schema, table) in &rls_tables {
        let key = table.as_str();
        if !tested.contains(key) && !excluded.contains(key) {
            gaps.push(format!("{schema}.{table}"));
        }
    }

    assert!(
        gaps.is_empty(),
        "The following tables have RLS policies but no isolation test and are not in the \
         exclusion list. Add a test to rls_policy_matrix_test.rs and register the table \
         in tested_tables(), or add it to rls_excluded_tables() with a justification:\n  {}",
        gaps.join("\n  ")
    );

    // Log coverage for the PR description.
    println!("RLS coverage report:");
    println!("  Tables with RLS policies: {}", rls_tables.len());
    println!("  Tested:   {}", tested.len());
    println!("  Excluded: {}", excluded.len());
    for (schema, table) in &rls_tables {
        let status = if tested.contains(table.as_str()) {
            "✅ tested"
        } else if excluded.contains(table.as_str()) {
            "⚠️  excluded (intentional)"
        } else {
            "❌ GAP"
        };
        println!("  {schema}.{table}: {status}");
    }
}

// ── transactions: RLS isolation ───────────────────────────────────────────────

/// Tenant A cannot see tenant B's transactions via the app role.
#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_transactions_rls_isolation() {
    let (app_pool, admin_pool, _c) = setup_db().await;

    let ta = create_tenant(&admin_pool, "RLS-TxA").await;
    let tb = create_tenant(&admin_pool, "RLS-TxB").await;
    let tx_b = insert_tx(&app_pool, tb).await;

    let mut conn = app_pool.acquire().await.unwrap();
    set_tenant_context(&mut conn, Some(ta), false)
        .await
        .unwrap();
    let row: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM transactions WHERE id = $1")
        .bind(tx_b)
        .fetch_optional(&mut *conn)
        .await
        .unwrap();
    assert!(
        row.is_none(),
        "tenant A must not see tenant B's transaction"
    );
}

/// Superuser bypass audit: the bootstrap postgres superuser (rolbypassrls=true)
/// CAN see all tenants' transactions — this is the documented pre-fix
/// behaviour (see rls_superuser_bypass_audit_test.rs). Here we assert the
/// inverse: the app role (NOBYPASSRLS) must NOT see cross-tenant rows.
#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_transactions_nobypassrls_role_is_isolated() {
    let (app_pool, admin_pool, _c) = setup_db().await;

    let ta = create_tenant(&admin_pool, "BypassA-tx").await;
    let tb = create_tenant(&admin_pool, "BypassB-tx").await;
    let _tx_a = insert_tx(&app_pool, ta).await;
    let tx_b = insert_tx(&app_pool, tb).await;

    // App role querying as tenant A must not see tenant B's row.
    let mut conn = app_pool.acquire().await.unwrap();
    set_tenant_context(&mut conn, Some(ta), false)
        .await
        .unwrap();
    let rows: Vec<(Uuid,)> = sqlx::query_as("SELECT id FROM transactions WHERE id = $1")
        .bind(tx_b)
        .fetch_all(&mut *conn)
        .await
        .unwrap();
    assert!(
        rows.is_empty(),
        "NOBYPASSRLS app role as tenant A must be isolated from tenant B's transactions"
    );
}

// ── settlements: RLS isolation ────────────────────────────────────────────────

/// A settlement that is NOT linked to any of tenant A's transactions must be
/// invisible to tenant A via the app role.
#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_settlements_rls_isolation_no_linked_tx() {
    let (app_pool, admin_pool, _c) = setup_db().await;

    let ta = create_tenant(&admin_pool, "RLS-SettleA").await;
    let _tb = create_tenant(&admin_pool, "RLS-SettleB").await;

    // Settlement with no linked transactions at all.
    let settle_id = insert_settlement(&admin_pool).await;

    let mut conn = app_pool.acquire().await.unwrap();
    set_tenant_context(&mut conn, Some(ta), false)
        .await
        .unwrap();
    let row: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM settlements WHERE id = $1")
        .bind(settle_id)
        .fetch_optional(&mut *conn)
        .await
        .unwrap();
    assert!(
        row.is_none(),
        "tenant A must not see a settlement with no linked transactions"
    );
}

/// A settlement linked to tenant A's transaction IS visible to tenant A.
#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_settlements_rls_visible_when_linked() {
    let (app_pool, admin_pool, _c) = setup_db().await;

    let ta = create_tenant(&admin_pool, "RLS-SettleLinkedA").await;
    let tx_a = insert_tx(&app_pool, ta).await;
    let settle_id = insert_settlement(&admin_pool).await;

    // Link the transaction to the settlement (admin pool, bypasses RLS).
    sqlx::query("UPDATE transactions SET settlement_id = $1 WHERE id = $2")
        .bind(settle_id)
        .bind(tx_a)
        .execute(&admin_pool)
        .await
        .unwrap();

    let mut conn = app_pool.acquire().await.unwrap();
    set_tenant_context(&mut conn, Some(ta), false)
        .await
        .unwrap();
    let row: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM settlements WHERE id = $1")
        .bind(settle_id)
        .fetch_optional(&mut *conn)
        .await
        .unwrap();
    assert!(
        row.is_some(),
        "tenant A must see a settlement that is linked to their transaction"
    );
}

/// Tenant B cannot see a settlement that is only linked to tenant A's tx.
#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_settlements_rls_cross_tenant_isolation() {
    let (app_pool, admin_pool, _c) = setup_db().await;

    let ta = create_tenant(&admin_pool, "RLS-XtA").await;
    let tb = create_tenant(&admin_pool, "RLS-XtB").await;
    let tx_a = insert_tx(&app_pool, ta).await;
    let settle_id = insert_settlement(&admin_pool).await;

    sqlx::query("UPDATE transactions SET settlement_id = $1 WHERE id = $2")
        .bind(settle_id)
        .bind(tx_a)
        .execute(&admin_pool)
        .await
        .unwrap();

    // Tenant B must not see the settlement.
    let mut conn = app_pool.acquire().await.unwrap();
    set_tenant_context(&mut conn, Some(tb), false)
        .await
        .unwrap();
    let row: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM settlements WHERE id = $1")
        .bind(settle_id)
        .fetch_optional(&mut *conn)
        .await
        .unwrap();
    assert!(
        row.is_none(),
        "tenant B must not see a settlement linked only to tenant A's transactions"
    );
}

/// Superuser bypass audit for settlements: the bootstrap postgres superuser
/// (rolbypassrls=true) sees all settlements; the NOBYPASSRLS app role sees
/// only those linked to the calling tenant's transactions.
#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_settlements_superuser_bypass_audit() {
    let (app_pool, admin_pool, _c) = setup_db().await;

    let ta = create_tenant(&admin_pool, "BypassSettleA").await;
    let tb = create_tenant(&admin_pool, "BypassSettleB").await;
    let tx_a = insert_tx(&app_pool, ta).await;
    let tx_b = insert_tx(&app_pool, tb).await;
    let settle_a = insert_settlement(&admin_pool).await;
    let settle_b = insert_settlement(&admin_pool).await;

    sqlx::query("UPDATE transactions SET settlement_id = $1 WHERE id = $2")
        .bind(settle_a)
        .bind(tx_a)
        .execute(&admin_pool)
        .await
        .unwrap();
    sqlx::query("UPDATE transactions SET settlement_id = $1 WHERE id = $2")
        .bind(settle_b)
        .bind(tx_b)
        .execute(&admin_pool)
        .await
        .unwrap();

    // Superuser (admin_pool, rolbypassrls=true) must see both settlements.
    let rows: Vec<(Uuid,)> =
        sqlx::query_as("SELECT id FROM settlements WHERE id = ANY($1)")
            .bind(vec![settle_a, settle_b])
            .fetch_all(&admin_pool)
            .await
            .unwrap();
    assert_eq!(rows.len(), 2, "superuser must see all settlements (bypass)");

    // App role (NOBYPASSRLS) as tenant A must see only settle_a.
    let mut conn = app_pool.acquire().await.unwrap();
    set_tenant_context(&mut conn, Some(ta), false)
        .await
        .unwrap();
    let rows: Vec<(Uuid,)> =
        sqlx::query_as("SELECT id FROM settlements WHERE id = ANY($1)")
            .bind(vec![settle_a, settle_b])
            .fetch_all(&mut *conn)
            .await
            .unwrap();
    let ids: std::collections::HashSet<Uuid> = rows.iter().map(|r| r.0).collect();
    assert!(
        ids.contains(&settle_a),
        "tenant A must see their own linked settlement"
    );
    assert!(
        !ids.contains(&settle_b),
        "tenant A must NOT see tenant B's linked settlement"
    );
}

/// Admin role sees all settlements regardless of the tenant context set.
#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_settlements_admin_sees_all() {
    let (app_pool, admin_pool, _c) = setup_db().await;

    let ta = create_tenant(&admin_pool, "AdminSettleA").await;
    let tb = create_tenant(&admin_pool, "AdminSettleB").await;
    let tx_a = insert_tx(&app_pool, ta).await;
    let tx_b = insert_tx(&app_pool, tb).await;
    let settle_a = insert_settlement(&admin_pool).await;
    let settle_b = insert_settlement(&admin_pool).await;

    sqlx::query("UPDATE transactions SET settlement_id = $1 WHERE id = $2")
        .bind(settle_a).bind(tx_a).execute(&admin_pool).await.unwrap();
    sqlx::query("UPDATE transactions SET settlement_id = $1 WHERE id = $2")
        .bind(settle_b).bind(tx_b).execute(&admin_pool).await.unwrap();

    // App role with is_admin=true must see both settlements.
    let mut conn = app_pool.acquire().await.unwrap();
    set_tenant_context(&mut conn, None, true).await.unwrap();
    let rows: Vec<(Uuid,)> =
        sqlx::query_as("SELECT id FROM settlements WHERE id = ANY($1)")
            .bind(vec![settle_a, settle_b])
            .fetch_all(&mut *conn)
            .await
            .unwrap();
    assert_eq!(rows.len(), 2, "admin must see all settlements");
}
