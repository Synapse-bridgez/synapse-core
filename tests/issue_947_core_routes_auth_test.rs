//! Integration tests for issue #947: core transaction/settlement read routes require authentication.
//!
//! Verifies that GET /transactions, /transactions/:id, /transactions/search,
//! /settlements, and /settlements/:id all enforce authentication middleware.

mod common;

use common::TestApp;
use reqwest::StatusCode;

/// Test that /transactions requires authentication
#[tokio::test]
async fn transactions_route_requires_auth() {
    let app = TestApp::new().await;
    let client = reqwest::Client::new();

    let res = client
        .get(format!("{}/transactions", app.base_url))
        .send()
        .await
        .unwrap();

    assert_eq!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "GET /transactions without auth should return 401 Unauthorized, \
         per issue #947 fix: core routes must require authentication"
    );
}

/// Test that /transactions/:id requires authentication
#[tokio::test]
async fn transaction_by_id_requires_auth() {
    let app = TestApp::new().await;
    let client = reqwest::Client::new();

    let res = client
        .get(format!(
            "{}/transactions/f47ac10b-58cc-4372-a567-0e02b2c3d479",
            app.base_url
        ))
        .send()
        .await
        .unwrap();

    assert_eq!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "GET /transactions/:id without auth should return 401 Unauthorized, \
         per issue #947 fix: core routes must require authentication"
    );
}

/// Test that /transactions/search requires authentication
#[tokio::test]
async fn transactions_search_requires_auth() {
    let app = TestApp::new().await;
    let client = reqwest::Client::new();

    let res = client
        .get(format!("{}/transactions/search?q=test", app.base_url))
        .send()
        .await
        .unwrap();

    assert_eq!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "GET /transactions/search without auth should return 401 Unauthorized, \
         per issue #947 fix: core routes must require authentication"
    );
}

/// Test that /settlements requires authentication
#[tokio::test]
async fn settlements_route_requires_auth() {
    let app = TestApp::new().await;
    let client = reqwest::Client::new();

    let res = client
        .get(format!("{}/settlements", app.base_url))
        .send()
        .await
        .unwrap();

    assert_eq!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "GET /settlements without auth should return 401 Unauthorized, \
         per issue #947 fix: core routes must require authentication"
    );
}

/// Test that /settlements/:id requires authentication
#[tokio::test]
async fn settlement_by_id_requires_auth() {
    let app = TestApp::new().await;
    let client = reqwest::Client::new();

    let res = client
        .get(format!(
            "{}/settlements/f47ac10b-58cc-4372-a567-0e02b2c3d479",
            app.base_url
        ))
        .send()
        .await
        .unwrap();

    assert_eq!(
        res.status(),
        StatusCode::UNAUTHORIZED,
        "GET /settlements/:id without auth should return 401 Unauthorized, \
         per issue #947 fix: core routes must require authentication"
    );
}
