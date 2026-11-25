//! Integration tests proving `GET /admin/audit/search` and the
//! `/admin/compliance/reports` endpoints are actually reachable over real
//! HTTP through the live router — not just that their handler functions
//! compile and behave correctly in isolation.
//!
//! Before this fix, `src/handlers/admin/audit.rs` and `compliance.rs` were
//! never `mod`-declared in `src/handlers/admin/mod.rs`, so they didn't even
//! compile into the crate; the only tests that existed for this area
//! asserted compilation, not routability, which is exactly the gap these
//! tests close. They go through `TestApp` (a real HTTP server bound to a
//! real Postgres testcontainer via `create_app`), not a direct call into
//! the handler function.
//!
//! All ignored (require Docker for the Postgres testcontainer); run with
//! `cargo test --test admin_audit_compliance_routes_test -- --ignored`.

mod common;

use common::TestApp;

const ADMIN_KEY: &str = "admin-audit-compliance-test-key";

fn admin_env() {
    std::env::set_var("ADMIN_API_KEY", ADMIN_KEY);
}

#[tokio::test]
#[ignore = "Requires Docker for Postgres testcontainer"]
async fn audit_search_requires_admin_auth() {
    admin_env();
    let app = TestApp::new().await;
    let client = reqwest::Client::new();

    let res = client
        .get(format!("{}/admin/audit/search", app.base_url))
        .send()
        .await
        .unwrap();

    assert_eq!(
        res.status(),
        401,
        "admin/audit/search must reject requests with no admin bearer token"
    );
}

#[tokio::test]
#[ignore = "Requires Docker for Postgres testcontainer"]
async fn audit_search_is_reachable_and_returns_valid_shape() {
    admin_env();
    let app = TestApp::new().await;
    let client = reqwest::Client::new();

    let res = client
        .get(format!("{}/admin/audit/search", app.base_url))
        .bearer_auth(ADMIN_KEY)
        .send()
        .await
        .unwrap();

    assert_eq!(
        res.status(),
        200,
        "admin/audit/search must be routable with a valid admin token — \
         a 404 here means the route regressed back to being unmounted"
    );

    let body: serde_json::Value = res.json().await.unwrap();
    assert!(body.get("total").is_some());
    assert!(body.get("data").is_some());
    assert!(body.get("next_cursor").is_some());
}

#[tokio::test]
#[ignore = "Requires Docker for Postgres testcontainer"]
async fn compliance_reports_requires_admin_auth() {
    admin_env();
    let app = TestApp::new().await;
    let client = reqwest::Client::new();

    let res = client
        .get(format!("{}/admin/compliance/reports", app.base_url))
        .send()
        .await
        .unwrap();

    assert_eq!(
        res.status(),
        401,
        "compliance/reports must reject requests with no admin bearer token"
    );
}

#[tokio::test]
#[ignore = "Requires Docker for Postgres testcontainer"]
async fn compliance_report_generate_then_list_round_trips_over_http() {
    admin_env();
    let app = TestApp::new().await;
    let client = reqwest::Client::new();

    let generate_res = client
        .post(format!(
            "{}/admin/compliance/reports?period=daily",
            app.base_url
        ))
        .bearer_auth(ADMIN_KEY)
        .send()
        .await
        .unwrap();

    assert_eq!(
        generate_res.status(),
        201,
        "compliance report generation must be routable with a valid admin token"
    );

    let list_res = client
        .get(format!("{}/admin/compliance/reports", app.base_url))
        .bearer_auth(ADMIN_KEY)
        .send()
        .await
        .unwrap();

    assert_eq!(list_res.status(), 200);
    let reports: serde_json::Value = list_res.json().await.unwrap();
    let reports = reports.as_array().expect("list_reports returns an array");
    assert!(
        !reports.is_empty(),
        "the report generated above must show up in the list endpoint over the same live path"
    );
}
