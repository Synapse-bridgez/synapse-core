//! #1334 — dependency scorecard persistence against a real Postgres.
//!
//! Exercises the upsert-merge (a period flushed in several parts sums into
//! one row, latency buckets element-wise), window loading across instances,
//! the scorecard built from persisted rows, and pruning.

use chrono::{Duration, TimeZone, Utc};
use synapse_core::services::dependency_scorecard::{
    build_scorecard, load_rollups, persist_rollups, prune_rollups, Dependency, PeriodRollup,
    LATENCY_BUCKETS,
};

async fn pool() -> sqlx::PgPool {
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://synapse:synapse@localhost:5432/synapse_test".to_string());
    let pool = sqlx::PgPool::connect(&url).await.expect("connect");
    sqlx::raw_sql(include_str!(
        "../migrations/20260927000000_dependency_health_rollups.sql"
    ))
    .execute(&pool)
    .await
    .expect("apply migration");
    pool
}

fn row(dep: Dependency, start: chrono::DateTime<Utc>, instance: &str) -> PeriodRollup {
    PeriodRollup {
        dependency: dep,
        period_start: start,
        instance_id: instance.to_string(),
        calls: 10,
        successes: 9,
        dependency_faults: 1,
        transport_failures: 0,
        circuit_rejected: 0,
        partition_failures: 0,
        minutes_observed: 5,
        minutes_up: 5,
        minutes_partitioned: 0,
        circuit_open_transitions: 0,
        latency_buckets: {
            let mut b = vec![0; LATENCY_BUCKETS];
            b[3] = 10;
            b
        },
    }
}

#[tokio::test]
#[ignore = "Requires Postgres (DATABASE_URL)"]
async fn upsert_merges_partial_flushes_and_loads_windows() {
    let pool = pool().await;
    let instance = format!("test-{}", uuid::Uuid::new_v4());
    let start = Utc.with_ymd_and_hms(2026, 9, 1, 10, 0, 0).unwrap();

    // Same period flushed twice: counters and buckets must add up.
    persist_rollups(&pool, &[row(Dependency::Redis, start, &instance)])
        .await
        .unwrap();
    let mut second = row(Dependency::Redis, start, &instance);
    second.latency_buckets[7] = 4;
    persist_rollups(&pool, &[second]).await.unwrap();
    persist_rollups(&pool, &[row(Dependency::Vault, start, &instance)])
        .await
        .unwrap();

    let rows: Vec<_> = load_rollups(
        &pool,
        start - Duration::minutes(1),
        start + Duration::minutes(5),
        Some(Dependency::Redis),
    )
    .await
    .unwrap()
    .into_iter()
    .filter(|r| r.instance_id == instance)
    .collect();
    assert_eq!(rows.len(), 1);
    let r = &rows[0];
    assert_eq!(r.calls, 20);
    assert_eq!(r.dependency_faults, 2);
    assert_eq!(r.minutes_up, 10);
    assert_eq!(r.latency_buckets[3], 20);
    assert_eq!(r.latency_buckets[7], 4);
    assert_eq!(r.latency_buckets.len(), LATENCY_BUCKETS);

    let all: Vec<_> = load_rollups(&pool, start, start + Duration::minutes(5), None)
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.instance_id == instance)
        .collect();
    assert_eq!(all.len(), 2);

    let card = build_scorecard(&all, start + Duration::hours(1), &[7]);
    let redis = card
        .entries
        .iter()
        .find(|e| e.dependency == Dependency::Redis)
        .unwrap();
    assert_eq!(redis.calls, 20);
    assert_eq!(redis.error_rate, Some(0.1));

    // Pruning removes old rows (these are far in the past relative to the cutoff).
    let pruned = prune_rollups(&pool, start + Duration::minutes(1))
        .await
        .unwrap();
    assert!(pruned >= 2);
    let left = load_rollups(&pool, start, start + Duration::minutes(5), None)
        .await
        .unwrap();
    assert!(left.iter().all(|r| r.instance_id != instance));
}
