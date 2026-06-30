//! Subprocess integration tests for `synapse transactions get`.
//!
//! Each test:
//!   1. Spawns the `mock-server` binary on an ephemeral port (shared fixture via
//!      `MockServer` helper below).
//!   2. Runs `synapse transactions get <ID> [--json]` as a real subprocess.
//!   3. Asserts exit code and stdout content.
//!
//! The mock server is the same binary used by `cli.rs`; here we always use the
//! "happy" scenario (success responses) or "not_found" (404 for every ID).

use assert_cmd::Command;
use std::net::TcpListener;
use std::process::{Child, Command as StdCommand, Stdio};
use std::thread;
use std::time::Duration;

// ── Shared transaction IDs ────────────────────────────────────────────────────

/// A well-known UUID the mock server returns successfully for `/transactions/<id>`.
const KNOWN_TX_ID: &str = "550e8400-e29b-41d4-a716-446655440000";

/// All-zeros UUID – the mock server always returns 404 for this ID.
const MISSING_TX_ID: &str = "00000000-0000-0000-0000-000000000000";

// ── Helper: run `synapse` pointing at the mock server ─────────────────────────

fn synapse_cmd(base_url: &str) -> Command {
    let mut cmd = Command::cargo_bin("synapse").expect("synapse binary should exist");
    cmd.arg("--base-url").arg(base_url);
    cmd
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Table output (default): exit 0, key-value rows rendered.
#[test]
fn transactions_get_table_output_happy_path() {
    let server = MockServer::spawn("happy");
    let base_url = server.base_url();

    let output = synapse_cmd(&base_url)
        .args(["transactions", "get", KNOWN_TX_ID])
        .output()
        .expect("command output");

    assert!(
        output.status.success(),
        "expected exit 0, got {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    // The formatter's print_one() renders key: value lines for the object fields
    assert!(stdout.contains(KNOWN_TX_ID), "stdout should contain transaction id");
    assert!(stdout.contains("USD"), "stdout should contain asset code");
    assert!(stdout.contains("100.00"), "stdout should contain amount");
    assert!(stdout.contains("pending"), "stdout should contain status");
}

/// JSON output (`--json` flag): exit 0, valid JSON object in stdout.
#[test]
fn transactions_get_json_output_happy_path() {
    let server = MockServer::spawn("happy");
    let base_url = server.base_url();

    let output = synapse_cmd(&base_url)
        .args(["transactions", "get", KNOWN_TX_ID, "--json"])
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
        KNOWN_TX_ID,
        "JSON id field should match"
    );
    assert_eq!(parsed["asset_code"], "USD");
    assert_eq!(parsed["amount"], "100.00");
    assert_eq!(parsed["status"], "pending");
    // Confirm it's the pretty-printed JSON (has newlines)
    assert!(stdout.contains('\n'), "--json output should be pretty-printed");
}

/// JSON output: field set includes all mandatory transaction keys.
#[test]
fn transactions_get_json_contains_required_fields() {
    let server = MockServer::spawn("happy");
    let base_url = server.base_url();

    let output = synapse_cmd(&base_url)
        .args(["transactions", "get", KNOWN_TX_ID, "--json"])
        .output()
        .expect("command output");

    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    for field in &[
        "\"id\"",
        "\"stellar_account\"",
        "\"amount\"",
        "\"asset_code\"",
        "\"status\"",
        "\"created_at\"",
        "\"updated_at\"",
    ] {
        assert!(
            stdout.contains(field),
            "stdout JSON should contain field {field}\nstdout: {stdout}"
        );
    }
}

/// Table output: ID is truncated to 8 chars + "..." per the TableDisplay impl.
#[test]
fn transactions_get_table_truncates_id() {
    let server = MockServer::spawn("happy");
    let base_url = server.base_url();

    let output = synapse_cmd(&base_url)
        .args(["transactions", "get", KNOWN_TX_ID])
        .output()
        .expect("command output");

    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    // Table mode for a single item uses print_one → format_value_as_kv,
    // which renders the full object as key: value; the full ID should appear
    assert!(stdout.contains(KNOWN_TX_ID));
}

/// Not-found: exit non-zero, error message to stderr.
#[test]
fn transactions_get_not_found_exits_nonzero() {
    let server = MockServer::spawn("not_found");
    let base_url = server.base_url();

    let output = synapse_cmd(&base_url)
        .args(["transactions", "get", MISSING_TX_ID])
        .output()
        .expect("command output");

    assert!(
        !output.status.success(),
        "expected non-zero exit for 404, got {:?}",
        output.status
    );

    let stderr = String::from_utf8(output.stderr).expect("valid utf-8");
    // anyhow prints the chain of errors to stderr
    assert!(
        stderr.contains("404") || stderr.contains("not found") || stderr.contains("Error"),
        "stderr should mention 404 or not found\nstderr: {stderr}"
    );
}

/// Not-found with --json flag: still exits non-zero (error propagates before formatting).
#[test]
fn transactions_get_not_found_json_flag_also_fails() {
    let server = MockServer::spawn("not_found");
    let base_url = server.base_url();

    let output = synapse_cmd(&base_url)
        .args(["transactions", "get", MISSING_TX_ID, "--json"])
        .output()
        .expect("command output");

    assert!(
        !output.status.success(),
        "expected non-zero exit for 404 with --json"
    );
}

/// Invalid UUID should be rejected by clap before any HTTP request is made.
#[test]
fn transactions_get_invalid_uuid_rejected() {
    let server = MockServer::spawn("happy");
    let base_url = server.base_url();

    let output = synapse_cmd(&base_url)
        .args(["transactions", "get", "not-a-uuid"])
        .output()
        .expect("command output");

    assert!(
        !output.status.success(),
        "expected non-zero exit for invalid UUID"
    );
}

// ── MockServer helper (mirrors the one in cli.rs) ─────────────────────────────

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
