//! Integration tests for the PITR (point-in-time-recovery) backup restore
//! admin API: POST /admin/backup/restore-pitr and
//! GET /admin/backup/restore-pitr/:job_id.
//!
//! ## Scope
//!
//! These tests exercise the full HTTP stack against a real, testcontainers
//! Postgres: admin-auth enforcement, target-timestamp validation against
//! `backup_verification_logs` coverage, job persistence and the async
//! `tokio::spawn` state machine, status polling, and audit-log writes.
//!
//! What they deliberately do **not** exercise is a real `pg_basebackup` +
//! WAL-replay restore. That needs a second Postgres data directory, the
//! `postgres`/`pg_ctl`/`pg_basebackup` server binaries, and filesystem
//! access to an archived WAL directory (see the doc comment at the top of
//! `scripts/pitr_restore.sh` for the full contract) — none of which a
//! single testcontainer or this CI environment provides, and actually
//! invoking it would mean tearing down/rebuilding a Postgres instance per
//! test run, which is exactly the kind of destructive, slow operation
//! integration tests shouldn't be doing.
//!
//! Instead, the job-lifecycle tests point `PITR_RESTORE_SCRIPT` at a small
//! stub executable that honors the same contract the real script does
//! (exit 0 + stdout => success, exit != 0 + stderr => failure). This still
//! exercises the real `tokio::process::Command` invocation and the entire
//! orchestration path around it identically to production; only the
//! restore mechanics inside the script differ.

mod common;

use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;
use tokio::sync::Mutex;
use uuid::Uuid;

const ADMIN_KEY: &str = "test-admin-key-for-pitr";

/// `PITR_RESTORE_SCRIPT` is process-global; serialize the tests that touch
/// it so they don't race each other (mirrors the pattern in
/// `src/db/audit.rs`'s own env-var tests). An async-aware mutex so it's
/// safe to hold across the `.await` points in the test bodies below.
static ENV_LOCK: Mutex<()> = Mutex::const_new(());

async fn seed_verified_backup(pool: &PgPool, filename: &str, latest_timestamp: DateTime<Utc>) {
    sqlx::query(
        "INSERT INTO backup_verification_logs \
         (backup_filename, verification_status, row_count, latest_timestamp) \
         VALUES ($1, 'verified', 100, $2)",
    )
    .bind(filename)
    .bind(latest_timestamp)
    .execute(pool)
    .await
    .unwrap();
}

fn write_fake_script(dir: &TempDir, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, body).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path
}

async fn poll_job_until_terminal(client: &reqwest::Client, poll_url: &str) -> Value {
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let resp = client
            .get(poll_url)
            .header("Authorization", format!("Bearer {ADMIN_KEY}"))
            .send()
            .await
            .unwrap();
        let body: Value = resp.json().await.unwrap();
        let status = body["status"].as_str().unwrap_or("");
        if status == "succeeded" || status == "failed" {
            return body;
        }
    }
    panic!("job did not reach a terminal state within the poll window");
}

#[ignore = "Requires Docker"]
#[tokio::test]
async fn test_restore_pitr_requires_admin_auth() {
    std::env::set_var("ADMIN_API_KEY", ADMIN_KEY);
    let app = common::TestApp::new().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{}/admin/backup/restore-pitr", app.base_url))
        .json(&json!({"target_timestamp": Utc::now().to_rfc3339(), "dry_run": true}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 401);
}

#[ignore = "Requires Docker"]
#[tokio::test]
async fn test_restore_pitr_rejects_when_no_verified_backups() {
    std::env::set_var("ADMIN_API_KEY", ADMIN_KEY);
    let app = common::TestApp::new().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{}/admin/backup/restore-pitr", app.base_url))
        .header("Authorization", format!("Bearer {ADMIN_KEY}"))
        .json(&json!({"target_timestamp": Utc::now().to_rfc3339(), "dry_run": true}))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    let detail = body["detail"].as_str().unwrap_or_default();
    assert!(
        detail.contains("no verified backups"),
        "unexpected error detail: {detail}"
    );
}

#[ignore = "Requires Docker"]
#[tokio::test]
async fn test_restore_pitr_rejects_out_of_range_timestamps() {
    std::env::set_var("ADMIN_API_KEY", ADMIN_KEY);
    let app = common::TestApp::new().await;

    let earliest = Utc::now() - Duration::days(10);
    let latest = Utc::now() - Duration::hours(6);
    seed_verified_backup(&app.pool, "backup_a.sql.gz", earliest).await;
    seed_verified_backup(&app.pool, "backup_b.sql.gz", latest).await;

    let client = reqwest::Client::new();

    // Before the earliest verified backup.
    let resp = client
        .post(format!("{}/admin/backup/restore-pitr", app.base_url))
        .header("Authorization", format!("Bearer {ADMIN_KEY}"))
        .json(&json!({
            "target_timestamp": (earliest - Duration::days(5)).to_rfc3339(),
            "dry_run": true,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert!(body["detail"]
        .as_str()
        .unwrap()
        .contains("before the earliest available recovery point"));

    // After the latest verified backup, but still in the past (distinct from
    // the future-timestamp check).
    let resp = client
        .post(format!("{}/admin/backup/restore-pitr", app.base_url))
        .header("Authorization", format!("Bearer {ADMIN_KEY}"))
        .json(&json!({
            "target_timestamp": (latest + Duration::hours(3)).to_rfc3339(),
            "dry_run": true,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert!(body["detail"]
        .as_str()
        .unwrap()
        .contains("after the latest available recovery point"));

    // Both rejected attempts must still be traceable in the audit log.
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_logs WHERE action = 'pitr_restore_rejected'",
    )
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(count, 2);
}

#[ignore = "Requires Docker"]
#[tokio::test]
async fn test_restore_pitr_dry_run_succeeds_and_writes_audit_log() {
    std::env::set_var("ADMIN_API_KEY", ADMIN_KEY);
    let app = common::TestApp::new().await;

    let earliest = Utc::now() - Duration::days(10);
    let latest = Utc::now() - Duration::hours(1);
    seed_verified_backup(&app.pool, "backup_a.sql.gz", earliest).await;
    seed_verified_backup(&app.pool, "backup_b.sql.gz", latest).await;

    let target = earliest + Duration::days(2);
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/admin/backup/restore-pitr", app.base_url))
        .header("Authorization", format!("Bearer {ADMIN_KEY}"))
        .json(&json!({
            "target_timestamp": target.to_rfc3339(),
            "dry_run": true,
            "requested_by": "alice",
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 202);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "succeeded");
    assert_eq!(body["dry_run"], true);
    assert!(body["detail"].as_str().unwrap().contains("dry run"));

    let job_id: Uuid = body["id"].as_str().unwrap().parse().unwrap();

    let rows = sqlx::query(
        "SELECT actor, action FROM audit_logs WHERE entity_id = $1 ORDER BY timestamp ASC",
    )
    .bind(job_id)
    .fetch_all(&app.pool)
    .await
    .unwrap();

    assert!(!rows.is_empty(), "expected at least one audit log entry");
    assert!(rows.iter().all(|r| r.get::<String, _>("actor") == "alice"));
    let actions: Vec<String> = rows.iter().map(|r| r.get::<String, _>("action")).collect();
    assert!(actions.contains(&"pitr_restore_submitted".to_string()));
    assert!(actions.contains(&"pitr_restore_completed".to_string()));
}

#[ignore = "Requires Docker"]
#[tokio::test]
async fn test_restore_pitr_job_lifecycle_succeeds_with_stub_executor() {
    let _guard = ENV_LOCK.lock().await;
    std::env::set_var("ADMIN_API_KEY", ADMIN_KEY);
    let script_dir = TempDir::new().unwrap();
    let script = write_fake_script(
        &script_dir,
        "fake_restore_success.sh",
        "#!/bin/sh\necho 'fake restore completed'\nexit 0\n",
    );
    std::env::set_var("PITR_RESTORE_SCRIPT", &script);

    let app = common::TestApp::new().await;
    let earliest = Utc::now() - Duration::days(10);
    let latest = Utc::now() - Duration::hours(1);
    seed_verified_backup(&app.pool, "backup_a.sql.gz", earliest).await;
    seed_verified_backup(&app.pool, "backup_b.sql.gz", latest).await;

    let target = earliest + Duration::days(1);
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/admin/backup/restore-pitr", app.base_url))
        .header("Authorization", format!("Bearer {ADMIN_KEY}"))
        .json(&json!({
            "target_timestamp": target.to_rfc3339(),
            "dry_run": false,
            "requested_by": "bob",
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 202);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "pending");
    let job_id = body["id"].as_str().unwrap().to_string();

    let poll_url = format!("{}/admin/backup/restore-pitr/{job_id}", app.base_url);
    let final_body = poll_job_until_terminal(&client, &poll_url).await;

    assert_eq!(final_body["status"], "succeeded");
    assert_eq!(final_body["detail"], "fake restore completed");

    let completed_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_logs WHERE entity_id = $1 AND action = 'pitr_restore_completed'",
    )
    .bind(Uuid::parse_str(&job_id).unwrap())
    .fetch_one(&app.pool)
    .await
    .unwrap();
    assert_eq!(completed_count, 1);

    std::env::remove_var("PITR_RESTORE_SCRIPT");
}

#[ignore = "Requires Docker"]
#[tokio::test]
async fn test_restore_pitr_job_lifecycle_records_failure_with_stub_executor() {
    let _guard = ENV_LOCK.lock().await;
    std::env::set_var("ADMIN_API_KEY", ADMIN_KEY);
    let script_dir = TempDir::new().unwrap();
    let script = write_fake_script(
        &script_dir,
        "fake_restore_failure.sh",
        "#!/bin/sh\necho 'simulated restore failure' >&2\nexit 1\n",
    );
    std::env::set_var("PITR_RESTORE_SCRIPT", &script);

    let app = common::TestApp::new().await;
    let earliest = Utc::now() - Duration::days(10);
    let latest = Utc::now() - Duration::hours(1);
    seed_verified_backup(&app.pool, "backup_a.sql.gz", earliest).await;
    seed_verified_backup(&app.pool, "backup_b.sql.gz", latest).await;

    let target = earliest + Duration::days(1);
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/admin/backup/restore-pitr", app.base_url))
        .header("Authorization", format!("Bearer {ADMIN_KEY}"))
        .json(&json!({
            "target_timestamp": target.to_rfc3339(),
            "dry_run": false,
            "requested_by": "carol",
        }))
        .send()
        .await
        .unwrap();

    let body: Value = resp.json().await.unwrap();
    let job_id = body["id"].as_str().unwrap().to_string();

    let poll_url = format!("{}/admin/backup/restore-pitr/{job_id}", app.base_url);
    let final_body = poll_job_until_terminal(&client, &poll_url).await;

    assert_eq!(final_body["status"], "failed");
    assert_eq!(final_body["error_message"], "simulated restore failure");

    std::env::remove_var("PITR_RESTORE_SCRIPT");
}

#[ignore = "Requires Docker"]
#[tokio::test]
async fn test_restore_pitr_unknown_job_id_returns_404() {
    std::env::set_var("ADMIN_API_KEY", ADMIN_KEY);
    let app = common::TestApp::new().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!(
            "{}/admin/backup/restore-pitr/{}",
            app.base_url,
            Uuid::new_v4()
        ))
        .header("Authorization", format!("Bearer {ADMIN_KEY}"))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 404);
}
