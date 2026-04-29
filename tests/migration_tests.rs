//! Migration safety tests (issue #25)
//!
//! For every `<timestamp>_<name>.sql` up-migration there must be a matching
//! `<timestamp>_<name>.down.sql` file.  The round-trip test spins up a
//! throwaway Postgres container, runs all up-migrations, inserts a small
//! amount of dummy data, then applies every down-migration in reverse order,
//! and finally re-runs all up-migrations to confirm the schema is intact.

use sqlx::{migrate::Migrator, PgPool};
use std::{
    fs,
    path::{Path, PathBuf},
};
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;

// ── helpers ───────────────────────────────────────────────────────────────────

fn migrations_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations")
}

/// Collect all up-migration stems (filename without extension) sorted by name.
fn up_migration_stems() -> Vec<String> {
    let dir = migrations_dir();
    let mut stems: Vec<String> = fs::read_dir(&dir)
        .expect("cannot read migrations dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|name| name.ends_with(".sql") && !name.ends_with(".down.sql"))
        .map(|name| name.trim_end_matches(".sql").to_string())
        .collect();
    stems.sort();
    stems
}

// ── convention enforcement ────────────────────────────────────────────────────

/// Every up-migration must have a corresponding `.down.sql` file.
#[test]
fn every_up_migration_has_a_down_migration() {
    let dir = migrations_dir();
    let stems = up_migration_stems();
    assert!(!stems.is_empty(), "No migration files found in {dir:?}");

    let mut missing: Vec<String> = Vec::new();
    for stem in &stems {
        let down_path = dir.join(format!("{stem}.down.sql"));
        if !down_path.exists() {
            missing.push(format!("{stem}.down.sql"));
        }
    }

    assert!(
        missing.is_empty(),
        "Missing down-migration files:\n{}",
        missing.join("\n")
    );
}

/// Down-migration files must not exist without a corresponding up-migration.
#[test]
fn no_orphan_down_migrations() {
    let dir = migrations_dir();
    let up_stems: std::collections::HashSet<String> = up_migration_stems().into_iter().collect();

    let orphans: Vec<String> = fs::read_dir(&dir)
        .expect("cannot read migrations dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|name| name.ends_with(".down.sql"))
        .map(|name| name.trim_end_matches(".down.sql").to_string())
        .filter(|stem| !up_stems.contains(stem))
        .collect();

    assert!(
        orphans.is_empty(),
        "Orphan down-migration files (no matching up-migration):\n{}",
        orphans.join("\n")
    );
}

/// Down-migration files must be non-empty.
#[test]
fn down_migrations_are_non_empty() {
    let dir = migrations_dir();
    let mut empty: Vec<String> = Vec::new();

    for stem in up_migration_stems() {
        let path = dir.join(format!("{stem}.down.sql"));
        if path.exists() {
            let content = fs::read_to_string(&path)
                .unwrap_or_default()
                .trim()
                .to_string();
            if content.is_empty() {
                empty.push(path.display().to_string());
            }
        }
    }

    assert!(
        empty.is_empty(),
        "Empty down-migration files:\n{}",
        empty.join("\n")
    );
}

// ── round-trip test ───────────────────────────────────────────────────────────

/// Spin up a real Postgres container and verify:
///   1. All up-migrations apply cleanly.
///   2. Dummy data can be inserted.
///   3. All down-migrations apply cleanly (in reverse order).
///   4. All up-migrations can be re-applied (schema integrity).
#[ignore = "Requires Docker"]
#[tokio::test]
async fn migration_round_trip() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let db_url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");

    let pool = PgPool::connect(&db_url).await.unwrap();

    // ── Step 1: run all up-migrations ─────────────────────────────────────────
    let migrator = Migrator::new(migrations_dir().as_path()).await.unwrap();
    migrator.run(&pool).await.expect("up-migrations failed");

    // ── Step 2: insert dummy data into stable tables ───────────────────────────
    insert_dummy_data(&pool).await;

    // ── Step 3: apply down-migrations in reverse order ────────────────────────
    let stems = up_migration_stems();
    let dir = migrations_dir();

    for stem in stems.iter().rev() {
        let down_sql = fs::read_to_string(dir.join(format!("{stem}.down.sql")))
            .unwrap_or_else(|_| panic!("cannot read {stem}.down.sql"));

        sqlx::raw_sql(&down_sql)
            .execute(&pool)
            .await
            .unwrap_or_else(|e| panic!("down-migration {stem} failed: {e}"));
    }

    // ── Step 4: re-run all up-migrations ──────────────────────────────────────
    // sqlx Migrator tracks applied migrations in _sqlx_migrations; after the
    // down pass the table itself is gone, so we reconnect to a fresh pool.
    drop(pool);
    let pool2 = PgPool::connect(&db_url).await.unwrap();
    let migrator2 = Migrator::new(migrations_dir().as_path()).await.unwrap();
    migrator2
        .run(&pool2)
        .await
        .expect("re-run of up-migrations after rollback failed");
}

/// Insert a small amount of dummy data so the down-migrations are tested
/// against a non-empty database.
async fn insert_dummy_data(pool: &PgPool) {
    // Ensure a current-month partition exists for the partitioned transactions table.
    sqlx::query(
        r#"
        DO $$
        DECLARE
            pname TEXT;
            s TEXT;
            e TEXT;
        BEGIN
            pname := 'transactions_y' || TO_CHAR(NOW(), 'YYYY') || 'm' || TO_CHAR(NOW(), 'MM');
            s := TO_CHAR(DATE_TRUNC('month', NOW()), 'YYYY-MM-DD');
            e := TO_CHAR(DATE_TRUNC('month', NOW()) + INTERVAL '1 month', 'YYYY-MM-DD');
            IF NOT EXISTS (SELECT 1 FROM pg_class WHERE relname = pname) THEN
                EXECUTE format(
                    'CREATE TABLE %I PARTITION OF transactions FOR VALUES FROM (%L) TO (%L)',
                    pname, s, e
                );
            END IF;
        END $$;
        "#,
    )
    .execute(pool)
    .await
    .expect("failed to create test partition");

    // Insert a transaction.
    sqlx::query(
        r#"
        INSERT INTO transactions (stellar_account, amount, asset_code, status)
        VALUES ('GABC1234567890123456789012345678901234567890123456789012', 100.0, 'USD', 'pending')
        "#,
    )
    .execute(pool)
    .await
    .expect("failed to insert dummy transaction");

    // Insert a feature flag.
    sqlx::query(
        "INSERT INTO feature_flags (name, enabled) VALUES ('test_flag', false) ON CONFLICT DO NOTHING",
    )
    .execute(pool)
    .await
    .expect("failed to insert dummy feature flag");

    // Insert an audit log entry.
    sqlx::query(
        r#"
        INSERT INTO audit_logs (entity_id, entity_type, action, actor)
        VALUES (gen_random_uuid(), 'transaction', 'created', 'test')
        "#,
    )
    .execute(pool)
    .await
    .expect("failed to insert dummy audit log");
}
