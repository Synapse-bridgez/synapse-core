//! Subprocess integration tests for `synapse settlements list` and
//! `synapse settlements get`.
//!
//! Each test:
//!   1. Spawns the `mock-server` binary on an ephemeral port (shared fixture via
//!      the `MockServer` helper at the bottom of this file).
//!   2. Runs the `synapse` binary as a real subprocess via `assert_cmd`.
//!   3. Asserts exit code and stdout content.
//!
//! Scenarios used:
//!   "happy"     — realistic success payloads
//!   "edge"      — empty settlements list (delegates to happy-path in mock server,
//!                 so we stub "edge" as well, which returns happy-path for settlements)
//!   "not_found" — 404 for all transactions and settlements IDs

use assert_cmd::Command;
use std::net::TcpListener;
use std::process::{Child, Command as StdCommand, Stdio};
use std::thread;
use std::time::Duration;

// ── Well-known IDs ────────────────────────────────────────────────────────────

/// UUID returned successfully by the mock server for `/settlements/<id>`.
const KNOWN_SETTLEMENT_ID: &str = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";

/// All-zeros UUID – the mock server returns 404 for this ID.
const MISSING_SETTLEMENT_ID: &str = "00000000-0000-0000-0000-000000000000";

// ── Helper: build a `synapse` command pointing at the mock server ─────────────

fn synapse_cmd(base_url: &str) -> Command {
    let mut cmd = Command::cargo_bin("synapse").expect("synapse binary should exist");
    cmd.arg("--base-url").arg(base_url);
    cmd
}

// ─────────────────────────────────────────────────────────────────────────────
// settlements list
// ─────────────────────────────────────────────────────────────────────────────

/// Default (table) output: exit 0, metadata line and column headers rendered.
#[test]
fn settlements_list_table_output_happy_path() {
    let server = MockServer::spawn("happy");
    let base_url = server.base_url();

    let output = synapse_cmd(&base_url)
        .args(["settlements", "list"])
        .output()
        .expect("command output");

    assert!(
        output.status.success(),
        "expected exit 0, got {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    // Header printed by the list runner
    assert!(
        stdout.contains("Settlements:"),
        "stdout should contain 'Settlements:'\nstdout: {stdout}"
    );
    // Column headers from TableDisplay::headers()
    assert!(stdout.contains("ID"), "stdout should contain ID header");
    assert!(stdout.contains("ASSET"), "stdout should contain ASSET header");
    assert!(stdout.contains("STATUS"), "stdout should contain STATUS header");
    // At least one data row from the mock server
    assert!(stdout.contains("USD") || stdout.contains("EUR"), "stdout should contain asset code");
}

/// JSON output (`--json` flag): exit 0, valid JSON object with `settlements` key.
#[test]
fn settlements_list_json_output_happy_path() {
    let server = MockServer::spawn("happy");
    let base_url = server.base_url();

    let output = synapse_cmd(&base_url)
        .args(["settlements", "list", "--json"])
        .output()
        .expect("command output");

    assert!(
        output.status.success(),
        "expected exit 0, got {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");

    // Must be parseable JSON
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout should be valid JSON");

    assert!(
        parsed["settlements"].is_array(),
        "JSON should have 'settlements' array\nparsed: {parsed}"
    );
    assert!(
        parsed["has_more"].is_boolean(),
        "JSON should have 'has_more' boolean"
    );

    let settlements = parsed["settlements"].as_array().unwrap();
    assert!(!settlements.is_empty(), "settlements array should be non-empty");
    // Verify first item has required fields
    let first = &settlements[0];
    assert!(first["id"].is_string(), "settlement should have id");
    assert!(first["asset_code"].is_string(), "settlement should have asset_code");
    assert!(first["total_amount"].is_string(), "settlement should have total_amount");
    assert!(first["status"].is_string(), "settlement should have status");
}

/// JSON output: `settlements` array contains required top-level keys.
#[test]
fn settlements_list_json_contains_required_keys() {
    let server = MockServer::spawn("happy");
    let base_url = server.base_url();

    let output = synapse_cmd(&base_url)
        .args(["settlements", "list", "--json"])
        .output()
        .expect("command output");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");

    for key in &[
        "\"settlements\"",
        "\"has_more\"",
        "\"next_cursor\"",
    ] {
        assert!(
            stdout.contains(key),
            "JSON output should contain key {key}\nstdout: {stdout}"
        );
    }
}

/// Table output respects `--limit` parameter (no error expected).
#[test]
fn settlements_list_table_with_limit() {
    let server = MockServer::spawn("happy");
    let base_url = server.base_url();

    let output = synapse_cmd(&base_url)
        .args(["settlements", "list", "--limit", "5"])
        .output()
        .expect("command output");

    assert!(
        output.status.success(),
        "expected exit 0 with --limit 5\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Table output respects `--direction` parameter.
#[test]
fn settlements_list_table_with_direction() {
    let server = MockServer::spawn("happy");
    let base_url = server.base_url();

    let output = synapse_cmd(&base_url)
        .args(["settlements", "list", "--direction", "backward"])
        .output()
        .expect("command output");

    assert!(
        output.status.success(),
        "expected exit 0 with --direction backward\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// settlements get
// ─────────────────────────────────────────────────────────────────────────────

/// Default (table) output: exit 0, key-value rows for all settlement fields.
#[test]
fn settlements_get_table_output_happy_path() {
    let server = MockServer::spawn("happy");
    let base_url = server.base_url();

    let output = synapse_cmd(&base_url)
        .args(["settlements", "get", KNOWN_SETTLEMENT_ID])
        .output()
        .expect("command output");

    assert!(
        output.status.success(),
        "expected exit 0, got {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(
        stdout.contains(KNOWN_SETTLEMENT_ID),
        "stdout should contain settlement id\nstdout: {stdout}"
    );
    assert!(stdout.contains("USD"), "stdout should contain asset_code");
    assert!(stdout.contains("14250.00"), "stdout should contain total_amount");
    assert!(stdout.contains("completed"), "stdout should contain status");
}

/// JSON output (`--json` flag): exit 0, well-formed JSON object.
#[test]
fn settlements_get_json_output_happy_path() {
    let server = MockServer::spawn("happy");
    let base_url = server.base_url();

    let output = synapse_cmd(&base_url)
        .args(["settlements", "get", KNOWN_SETTLEMENT_ID, "--json"])
        .output()
        .expect("command output");

    assert!(
        output.status.success(),
        "expected exit 0, got {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");

    // Must be parseable JSON
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout should be valid JSON");

    assert_eq!(
        parsed["id"].as_str().unwrap_or_default(),
        KNOWN_SETTLEMENT_ID,
        "JSON id should match"
    );
    assert_eq!(parsed["asset_code"], "USD");
    assert_eq!(parsed["total_amount"], "14250.00");
    assert_eq!(parsed["status"], "completed");
    assert_eq!(parsed["tx_count"], 47);

    // Pretty-printed (has newlines)
    assert!(stdout.contains('\n'), "--json output should be pretty-printed");
}

/// JSON output: all required settlement fields present.
#[test]
fn settlements_get_json_contains_required_fields() {
    let server = MockServer::spawn("happy");
    let base_url = server.base_url();

    let output = synapse_cmd(&base_url)
        .args(["settlements", "get", KNOWN_SETTLEMENT_ID, "--json"])
        .output()
        .expect("command output");

    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    for field in &[
        "\"id\"",
        "\"asset_code\"",
        "\"total_amount\"",
        "\"tx_count\"",
        "\"status\"",
        "\"period_start\"",
        "\"period_end\"",
        "\"created_at\"",
        "\"updated_at\"",
    ] {
        assert!(
            stdout.contains(field),
            "stdout JSON should contain field {field}\nstdout: {stdout}"
        );
    }
}

/// Not-found: exit non-zero, error on stderr.
#[test]
fn settlements_get_not_found_exits_nonzero() {
    let server = MockServer::spawn("not_found");
    let base_url = server.base_url();

    let output = synapse_cmd(&base_url)
        .args(["settlements", "get", MISSING_SETTLEMENT_ID])
        .output()
        .expect("command output");

    assert!(
        !output.status.success(),
        "expected non-zero exit for 404 settlement, got {:?}",
        output.status
    );

    let stderr = String::from_utf8(output.stderr).expect("valid utf-8");
    assert!(
        stderr.contains("404") || stderr.contains("not found") || stderr.contains("Error"),
        "stderr should mention 404 or not found\nstderr: {stderr}"
    );
}

/// Not-found with --json: still non-zero (error happens before formatting).
#[test]
fn settlements_get_not_found_json_flag_also_fails() {
    let server = MockServer::spawn("not_found");
    let base_url = server.base_url();

    let output = synapse_cmd(&base_url)
        .args(["settlements", "get", MISSING_SETTLEMENT_ID, "--json"])
        .output()
        .expect("command output");

    assert!(
        !output.status.success(),
        "expected non-zero exit for 404 with --json"
    );
}

/// Invalid UUID is rejected by clap before any HTTP request is made.
#[test]
fn settlements_get_invalid_uuid_rejected() {
    let server = MockServer::spawn("happy");
    let base_url = server.base_url();

    let output = synapse_cmd(&base_url)
        .args(["settlements", "get", "not-a-uuid"])
        .output()
        .expect("command output");

    assert!(
        !output.status.success(),
        "expected non-zero exit for invalid UUID"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// MockServer helper (mirrors cli.rs)
// ─────────────────────────────────────────────────────────────────────────────

struct MockServer {
    child: Child,
    port: u16,
}

impl MockServer {
    fn spawn(scenario: &str) -> Self {
        let port = free_port();
        let binary = std::env::var_os("CARGO_BIN_EXE_mock-server")
            .expect("CARGO_BIN_EXE_mock-server must be set (run via cargo test)");

        let child = StdCommand::new(binary)
            .env("MOCK_SERVER_ADDR", format!("127.0.0.1:{port}"))
            .env("MOCK_SERVER_SCENARIO", scenario)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("mock-server binary should start");

        wait_for_port(port);
        Self { child, port }
    }

    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral port")
        .local_addr()
        .expect("local addr")
        .port()
}

fn wait_for_port(port: u16) {
    for _ in 0..50 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("mock server did not start on port {port}");
}
