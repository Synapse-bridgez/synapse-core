//! Reusable mock-server utilities for testing code that talks to the
//! Synapse API, built on [`wiremock`].
//!
//! This is the same helper this repository's own CLI test suite uses
//! internally, published here so third-party integrators can write tests
//! against a realistic double of the Synapse API instead of hitting a real
//! staging environment or hand-rolling their own mocks.
//!
//! Enable it with the `testing-support` feature:
//!
//! ```toml
//! [dev-dependencies]
//! synapse-sdk = { version = "*", features = ["testing-support"] }
//! ```
//!
//! # Example
//!
//! ```no_run
//! # async fn example() {
//! use synapse_sdk::testing::{spawn_mock_server, stub_endpoint};
//! use serde_json::json;
//!
//! let server = spawn_mock_server().await;
//! stub_endpoint(&server, "GET", "/v1/transactions/123", 200, json!({"id": "123"})).await;
//! # }
//! ```
//!
//! Error responses can be built directly from this crate's own
//! [`CatalogEntry`](crate::error::CatalogEntry) model (the same type the SDK
//! deserializes `GET /errors` into), so a mock's error shape can't silently
//! drift from what the real API actually returns:
//!
//! ```no_run
//! # async fn example() {
//! use synapse_sdk::testing::{spawn_mock_server, stub_error};
//! use synapse_sdk::error::CatalogEntry;
//!
//! let server = spawn_mock_server().await;
//! let entry = CatalogEntry {
//!     code: "invalid_cursor".into(),
//!     http_status: 400,
//!     description: "the pagination cursor was invalid or expired".into(),
//! };
//! stub_error(&server, "GET", "/v1/transactions", &entry).await;
//! # }
//! ```

use crate::error::CatalogEntry;
use wiremock::matchers::{method as method_matcher, path as path_matcher};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Start a fresh in-process mock HTTP server.
pub async fn spawn_mock_server() -> MockServer {
    MockServer::start().await
}

/// Stub `method path` on `server` to return `status` with `body` as JSON.
pub async fn stub_endpoint(
    server: &MockServer,
    method: &str,
    path: &str,
    status: u16,
    body: serde_json::Value,
) {
    Mock::given(method_matcher(method))
        .and(path_matcher(path))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(server)
        .await;
}

/// Stub `method path` to fail with a [`CatalogEntry`] from the real API's
/// error catalog, in the same `{"code", "error"}` shape the live API uses.
///
/// Building the response from `CatalogEntry` (rather than a hand-written
/// JSON literal) ties the mock to the same model the SDK itself decodes
/// `GET /errors` into, so the two cannot independently drift apart.
pub async fn stub_error(server: &MockServer, method: &str, path: &str, entry: &CatalogEntry) {
    let body = serde_json::json!({
        "code": entry.code,
        "error": entry.description,
    });
    stub_endpoint(server, method, path, entry.http_status, body).await;
}

/// Stub `method path` to return `429 Too Many Requests` with a
/// `Retry-After: retry_after_secs` header, for testing rate-limit handling
/// (e.g. that a client honors `Retry-After` — see `synapse_sdk::retry`).
pub async fn stub_rate_limited(
    server: &MockServer,
    method: &str,
    path: &str,
    retry_after_secs: u64,
    message: &str,
) {
    let template = ResponseTemplate::new(429)
        .insert_header("Retry-After", retry_after_secs.to_string())
        .set_body_json(serde_json::json!({ "error": message }));
    Mock::given(method_matcher(method))
        .and(path_matcher(path))
        .respond_with(template)
        .mount(server)
        .await;
}
