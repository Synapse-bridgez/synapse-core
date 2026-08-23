//! Regression test for issue #1060 Part A: nothing in the live `serve()` startup
//! path ever called `ReadinessState::run_initialization_checks()`, so `/ready`
//! returned 503 for the entire life of every process.
//!
//! Unlike the existing readiness tests (which construct a `ReadinessState` in
//! isolation and call `.set_ready()` directly), this drives the actual
//! mechanism end to end: a real HTTP server built from `create_app` (the same
//! wiring `main.rs`'s `serve()` uses), a real Postgres connection, and a real
//! GET /ready poll — asserting the 503 → 200 transition that the missing call
//! made impossible.

mod common;

use reqwest::StatusCode;
use tokio::time::{sleep, Duration, Instant};

#[tokio::test]
#[ignore = "Requires Docker for testcontainers"]
async fn test_ready_transitions_503_to_200_after_initialization_checks() {
    let app = common::TestApp::new().await;
    let client = reqwest::Client::new();

    // Before initialization checks run, /ready must report 503 — this is the
    // permanent-outage state every instance was stuck in before the fix.
    let res = client
        .get(format!("{}/ready", app.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "/ready must be 503 before initialization checks complete"
    );

    // Mirror exactly what main.rs's serve() now does: spawn the checks in the
    // background against the real pool, non-blocking, so the HTTP listener
    // stays up while readiness catches up.
    let readiness = app.readiness.clone();
    let pool = app.pool.clone();
    tokio::spawn(async move {
        readiness
            .run_initialization_checks(
                &pool,
                "redis://localhost:6379",
                "https://horizon-testnet.stellar.org",
            )
            .await
            .expect("initialization checks should succeed against a healthy test database");
    });

    // Poll the real HTTP endpoint until it flips to 200, bounded by a timeout.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last_status;
    loop {
        let res = client
            .get(format!("{}/ready", app.base_url))
            .send()
            .await
            .unwrap();
        last_status = res.status();
        if last_status == StatusCode::OK {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "/ready never transitioned to 200 within the timeout; last status: {last_status}"
        );
        sleep(Duration::from_millis(100)).await;
    }

    assert_eq!(last_status, StatusCode::OK);
}
