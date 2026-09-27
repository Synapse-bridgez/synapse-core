//! Integration tests for `synapse graphql query` (Issue #87).
//!
//! These tests spin up a wiremock HTTP server, stub `POST /graphql`, then run
//! the `synapse` binary as a subprocess and assert on both the process exit
//! code and the printed output (table mode and --format json mode).
//!
//! Covers:
//!   - Happy path: list transactions query → table output (exit 0)
//!   - Happy path: single transaction query → JSON output (exit 0)
//!   - Edge case: HTTP 200 with `errors` array → exit 1, error message on stderr
//!   - HTTP 400 (unsupported query) → non-zero exit, error on stderr

mod support;

use assert_cmd::Command;
use predicates::prelude::*;
use support::spawn_mock_server;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// ── helpers ───────────────────────────────────────────────────────────────────

fn synapse_cmd() -> Command {
    Command::cargo_bin("synapse").expect("synapse binary must be compiled")
}

async fn stub_graphql(server: &MockServer, status: u16, body: serde_json::Value) {
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(server)
        .await;
}

// ── happy path: table output ──────────────────────────────────────────────────

/// `synapse graphql query` with the transactions query succeeds (exit 0) and
/// prints the transaction list in the default table format.
#[tokio::test]
async fn graphql_query_list_transactions_table_output_exit_0() {
    let server = spawn_mock_server().await;

    stub_graphql(
        &server,
        200,
        serde_json::json!({
            "data": {
                "transactions": [
                    { "id": "550e8400-e29b-41d4-a716-446655440000", "status": "pending" },
                    { "id": "550e8400-e29b-41d4-a716-446655440001", "status": "completed" }
                ]
            }
        }),
    )
    .await;

    let mut cmd = synapse_cmd();
    cmd.arg("--url")
        .arg(server.uri())
        .arg("graphql")
        .arg("query")
        .arg("--query")
        .arg("{ transactions { id status } }");

    cmd.assert()
        .success()
        // In table mode, the formatter shows the top-level keys of the response.
        // The GraphQL response has a "data" key containing the transactions object.
        .stdout(predicate::str::contains("data"));
}

// ── happy path: JSON output ───────────────────────────────────────────────────

/// `synapse graphql query --format json` returns pretty-printed JSON (exit 0).
#[tokio::test]
async fn graphql_query_json_output_exit_0() {
    let server = spawn_mock_server().await;

    stub_graphql(
        &server,
        200,
        serde_json::json!({
            "data": {
                "transaction": {
                    "id": "550e8400-e29b-41d4-a716-446655440000",
                    "status": "pending",
                    "amount": "100.00",
                    "assetCode": "USD"
                }
            }
        }),
    )
    .await;

    let mut cmd = synapse_cmd();
    cmd.arg("--url")
        .arg(server.uri())
        .arg("graphql")
        .arg("query")
        .arg("--query")
        .arg(r#"{ transaction(id: "550e8400-e29b-41d4-a716-446655440000") { id status amount assetCode } }"#)
        .arg("--format")
        .arg("json");

    cmd.assert()
        .success()
        // Pretty-printed JSON: must contain both a key and the transaction id
        .stdout(predicate::str::contains("\"data\""))
        .stdout(predicate::str::contains(
            "550e8400-e29b-41d4-a716-446655440000",
        ))
        .stdout(predicate::str::contains("\"status\""))
        .stdout(predicate::str::contains("pending"));
}

// ── edge case: GraphQL application error (HTTP 200 + errors array) ────────────

/// When the server returns HTTP 200 but includes a top-level `errors` array
/// the CLI must exit with a non-zero code and print the error message to stderr.
///
/// This is the documented edge case from the handler: an unsupported query
/// produces `{ "errors": [{ "message": "Unsupported GraphQL query" }] }` with
/// a 200 status, which the CLI must surface as a failure.
#[tokio::test]
async fn graphql_query_application_level_error_exits_nonzero() {
    let server = spawn_mock_server().await;

    stub_graphql(
        &server,
        200,
        serde_json::json!({
            "errors": [
                { "message": "Unsupported GraphQL query" }
            ]
        }),
    )
    .await;

    let mut cmd = synapse_cmd();
    cmd.arg("--url")
        .arg(server.uri())
        .arg("graphql")
        .arg("query")
        .arg("--query")
        .arg("{ unsupportedField }");

    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("Unsupported GraphQL query"));
}

// ── HTTP 400 (server rejects the request) ────────────────────────────────────

/// A 400 response (e.g. completely malformed query body) must also surface as
/// a non-zero exit with an error description on stderr.
#[tokio::test]
async fn graphql_query_http_400_exits_nonzero() {
    let server = spawn_mock_server().await;

    stub_graphql(&server, 400, serde_json::json!({ "error": "bad request" })).await;

    let mut cmd = synapse_cmd();
    cmd.arg("--url")
        .arg(server.uri())
        .arg("graphql")
        .arg("query")
        .arg("--query")
        .arg("not a valid query");

    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("400").or(predicate::str::contains("error")));
}

// ── help text sanity check ────────────────────────────────────────────────────

/// `synapse graphql query --help` must succeed and mention both `--query` and
/// `--format` so users can discover the interface.
#[test]
fn graphql_query_help_mentions_flags() {
    let mut cmd = synapse_cmd();
    cmd.arg("graphql").arg("query").arg("--help");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("--query"))
        .stdout(predicate::str::contains("--format"));
}

// ── JSON output contains no table formatting noise ────────────────────────────

/// In `--format json` mode the output must be valid JSON (parseable), not a
/// table string.
#[tokio::test]
async fn graphql_query_json_output_is_valid_json() {
    let server = spawn_mock_server().await;

    stub_graphql(
        &server,
        200,
        serde_json::json!({
            "data": {
                "transactions": [
                    { "id": "aaa", "status": "pending" }
                ]
            }
        }),
    )
    .await;

    let mut cmd = synapse_cmd();
    cmd.arg("--url")
        .arg(server.uri())
        .arg("graphql")
        .arg("query")
        .arg("--query")
        .arg("{ transactions { id status } }")
        .arg("--format")
        .arg("json");

    let output = cmd.output().expect("command must run");
    assert!(output.status.success(), "expected exit 0");

    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    // Must parse cleanly as JSON
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout must be valid JSON");
    assert!(parsed.get("data").is_some(), "JSON must contain 'data' key");
}
