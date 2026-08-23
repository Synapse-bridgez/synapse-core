//! Part B regression test: a request recorded via the database fallback
//! during a Redis outage must be recognized by the healthy-path lookup once
//! Redis recovers, rather than being treated as new and double-executed.

use sqlx::{migrate::Migrator, PgPool};
use std::path::Path;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use synapse_core::middleware::idempotency::{
    BodyEncoding, CachedResponse, IdempotencyService, IdempotencyStatus,
};
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

fn make_service(redis_url: &str, pool: PgPool) -> IdempotencyService {
    IdempotencyService::new(
        redis_url,
        pool,
        Arc::new(AtomicU64::new(0)),
        Arc::new(AtomicU64::new(0)),
        Arc::new(AtomicU64::new(0)),
        Arc::new(AtomicU64::new(0)),
        Arc::new(AtomicU64::new(0)),
        Arc::new(AtomicU64::new(0)),
    )
    .unwrap()
}

/// Drives the exact sequence from the bug report: a request recorded via the
/// DB fallback while Redis is down, then a same-key retry after Redis has
/// recovered. Before the fix, the retry's Redis GET misses (Redis never saw
/// the key) and a fresh lock is issued, i.e. `IdempotencyStatus::New` again —
/// the caller's handler would run a second time. After the fix, the healthy
/// path consults the DB fallback table on a Redis miss and returns
/// `Completed` with the original response instead.
#[ignore = "Requires Docker and Redis"]
#[tokio::test]
async fn test_retry_after_redis_recovery_is_recognized_not_reexecuted() {
    let (pool, _container) = setup_test_db().await;
    let tenant_id = "recovery-test-tenant";
    let key = uuid::Uuid::new_v4().to_string();

    // --- Phase 1: Redis is down. Request 1 arrives and is recorded via the DB fallback. ---
    let degraded_service =
        make_service("redis://invalid-host-simulating-outage:9999", pool.clone());

    let first_status = degraded_service
        .check_idempotency(tenant_id, &key)
        .await
        .unwrap();
    assert!(
        matches!(first_status, IdempotencyStatus::New { lock_token: None }),
        "expected the degraded path to record a fresh DB-fallback row, got {:?}",
        first_status
    );

    let original_response = CachedResponse {
        status: 200,
        body: r#"{"result":"processed-once"}"#.to_string(),
        content_type: Some("application/json".to_string()),
        encoding: BodyEncoding::Utf8,
    };
    degraded_service
        .store_response(tenant_id, &key, original_response.clone(), None)
        .await
        .unwrap();

    // --- Phase 2: Redis recovers. A caller retries with the same key. ---
    let redis_url =
        std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
    let recovered_service = make_service(&redis_url, pool.clone());

    let retry_status = recovered_service
        .check_idempotency(tenant_id, &key)
        .await
        .unwrap();

    match retry_status {
        IdempotencyStatus::Completed(cached) => {
            assert_eq!(cached.body, original_response.body);
            assert_eq!(cached.status, original_response.status);
        }
        other => panic!(
            "expected the retry to be recognized via the DB fallback as Completed, \
             but got {:?} — this means the handler would run a second time",
            other
        ),
    }
}
