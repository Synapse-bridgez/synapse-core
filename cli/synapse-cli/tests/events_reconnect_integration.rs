//! Integration tests for `synapse admin events reconnect` and
//! `synapse admin events reconnect-status`.
//!
//! These tests spin up the mock HTTP server binary (Issue #64) and run the
//! compiled `synapse` binary (Issue #92) as a subprocess, asserting on the
//! process exit code and on the content printed to stdout in both table mode
//! (default) and `--json` mode.
//!
//! # Scenarios tested
//!
//! ## `events reconnect`
//! - Happy path, table mode: exits 0, table rows contain type/status/session.
//! - Happy path, JSON mode: exits 0, output is valid JSON with expected fields.
//! - Edge case (session_expired): exits 0, printed status is `session_expired`.
//! - Server 5xx: exits non-zero (network/server error propagates as exit 1).
//!
//! ## `events reconnect-status`
//! - Happy path with `--cursor`, table mode: exits 0, status `ready`.
//! - Happy path with `--cursor`, JSON mode: exits 0, valid JSON.
//! - Edge case: no `--cursor` supplied is valid; server returns `ready`, exits 0.
//! - Edge case (session_expired via `--cursor`): exits 0, status `session_expired`.

use assert_cmd::Command;
use std::net::TcpListener;
use std::process::{Child, Command as StdCommand, Stdio};
use std::thread;
use std::time::Duration;

const SAMPLE_SESSION_ID: &str = "550e8400-e29b-41d4-a716-446655440000";

// ── Mock server harness ───────────────────────────────────────────────────────

struct MockServer {
    child: Child,
    port: u16,
}

impl MockServer {
    /// Spawn the mock-server binary with the given scenario.
    ///
    /// `scenario` is passed as `MOCK_SERVER_SCENARIO`; use `"happy"` for the
    /// normal happy path or `"edge"` for edge-case responses.
    fn spawn(scenario: &str) -> Self {
        let port = free_port();
        let binary = std::env::var_os("CARGO_BIN_EXE_mock-server")
            .expect("CARGO_BIN_EXE_mock-server must be set; run via `cargo test`");
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

// ── Helpers ───────────────────────────────────────────────────────────────────

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

fn synapse_cmd() -> Command {
    Command::cargo_bin("synapse").expect("synapse binary must exist")
}

// ═══════════════════════════════════════════════════════════════════════════════
// events reconnect
// ═══════════════════════════════════════════════════════════════════════════════

// ── Happy path: table mode ────────────────────────────────────────────────────

/// `synapse admin events reconnect --cursor <UUID>` exits 0 and prints a
/// key-value table containing `type`, `status`, `session_id`, and
/// `backoff_seconds` populated from the mock server's canned response.
#[test]
fn events_reconnect_happy_path_table() {
    let server = MockServer::spawn("happy");

    let output = synapse_cmd()
        .args([
            "--base-url",
            &server.base_url(),
            "admin",
            "events",
            "reconnect",
            "--cursor",
            SAMPLE_SESSION_ID,
        ])
        .output()
        .expect("command output");

    assert!(
        output.status.success(),
        "expected exit 0, got {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");

    // The table renderer prints key-value rows.
    assert!(
        stdout.contains("reconnect"),
        "expected 'reconnect' type in table output: {stdout}"
    );
    assert!(
        stdout.contains("ready"),
        "expected status 'ready' in table output: {stdout}"
    );
    assert!(
        stdout.contains(SAMPLE_SESSION_ID),
        "expected session_id '{SAMPLE_SESSION_ID}' in table output: {stdout}"
    );
    // backoff_seconds must appear as a number (5 from happy scenario).
    assert!(
        stdout.contains('5'),
        "expected backoff_seconds 5 in table output: {stdout}"
    );
}

// ── Happy path: JSON mode ─────────────────────────────────────────────────────

/// `synapse admin events reconnect --cursor <UUID> --json` exits 0 and emits a
/// JSON object with `type`, `status.status`, `status.session_id`, and
/// `backoff_seconds`.
#[test]
fn events_reconnect_happy_path_json() {
    let server = MockServer::spawn("happy");

    let output = synapse_cmd()
        .args([
            "--base-url",
            &server.base_url(),
            "admin",
            "events",
            "reconnect",
            "--cursor",
            SAMPLE_SESSION_ID,
            "--json",
        ])
        .output()
        .expect("command output");

    assert!(
        output.status.success(),
        "expected exit 0, got {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");

    // Must be a valid JSON object.
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout should be valid JSON");
    assert!(parsed.is_object(), "expected JSON object, got: {stdout}");

    assert_eq!(parsed["type"], "reconnect", "type mismatch: {stdout}");
    assert_eq!(
        parsed["status"]["status"], "ready",
        "status.status mismatch: {stdout}"
    );
    assert_eq!(
        parsed["status"]["session_id"], SAMPLE_SESSION_ID,
        "status.session_id mismatch: {stdout}"
    );
    assert_eq!(
        parsed["backoff_seconds"], 5,
        "backoff_seconds mismatch: {stdout}"
    );
    assert_eq!(
        parsed["requires_resync"], false,
        "requires_resync mismatch: {stdout}"
    );
}

// ── Edge case: session_expired response is not an error ───────────────────────

/// When the server returns `status: session_expired` (edge scenario), the
/// command must still exit 0 — the expired-session state is a valid outcome,
/// not a failure.
///
/// This is the documented edge case from Issue #92.
#[test]
fn events_reconnect_session_expired_is_not_an_error() {
    let server = MockServer::spawn("edge");

    let output = synapse_cmd()
        .args([
            "--base-url",
            &server.base_url(),
            "admin",
            "events",
            "reconnect",
            "--cursor",
            SAMPLE_SESSION_ID,
        ])
        .output()
        .expect("command output");

    assert!(
        output.status.success(),
        "session_expired response must exit 0, got {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(
        stdout.contains("session_expired"),
        "expected 'session_expired' in output: {stdout}"
    );
}

/// JSON mode: `session_expired` is still exit 0, output is valid JSON.
#[test]
fn events_reconnect_session_expired_json_mode() {
    let server = MockServer::spawn("edge");

    let output = synapse_cmd()
        .args([
            "--base-url",
            &server.base_url(),
            "admin",
            "events",
            "reconnect",
            "--cursor",
            SAMPLE_SESSION_ID,
            "--json",
        ])
        .output()
        .expect("command output");

    assert!(
        output.status.success(),
        "session_expired --json must exit 0, got {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout should be valid JSON");

    assert_eq!(parsed["type"], "reconnect");
    assert_eq!(parsed["status"]["status"], "session_expired");
    assert_eq!(parsed["backoff_seconds"], 0);
    assert_eq!(parsed["requires_resync"], false);
}

// ── Server error propagates as non-zero exit ──────────────────────────────────

/// A genuine 5xx response from the server must cause the CLI to exit non-zero.
/// We serve exactly one HTTP 500 via a minimal inline TCP server so we do not
/// need to add a dedicated "error" scenario to the mock-server binary.
#[test]
fn events_reconnect_server_error_exits_nonzero() {
    let port = free_port();
    let listener = TcpListener::bind(format!("127.0.0.1:{port}")).expect("bind");

    let handle = thread::spawn(move || {
        use std::io::{BufRead, BufReader, Write};
        if let Ok((mut stream, _)) = listener.accept() {
            // Drain request.
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
            }
            let body = r#"{"error":"internal server error"}"#;
            let resp = format!(
                "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(resp.as_bytes()).unwrap();
        }
    });

    wait_for_port(port);
    let base_url = format!("http://127.0.0.1:{port}");

    let output = synapse_cmd()
        .args([
            "--base-url",
            &base_url,
            "admin",
            "events",
            "reconnect",
            "--cursor",
            SAMPLE_SESSION_ID,
        ])
        .output()
        .expect("command output");

    let _ = handle.join();

    assert!(
        !output.status.success(),
        "server 500 must exit non-zero, got {:?}",
        output.status
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// events reconnect-status
// ═══════════════════════════════════════════════════════════════════════════════

// ── Happy path with --cursor: table mode ─────────────────────────────────────

/// `synapse admin events reconnect-status --cursor <UUID>` exits 0 and prints
/// a key-value table with the server's ready response.
#[test]
fn events_reconnect_status_with_cursor_happy_path_table() {
    let server = MockServer::spawn("happy");

    let output = synapse_cmd()
        .args([
            "--base-url",
            &server.base_url(),
            "admin",
            "events",
            "reconnect-status",
            "--cursor",
            SAMPLE_SESSION_ID,
        ])
        .output()
        .expect("command output");

    assert!(
        output.status.success(),
        "expected exit 0, got {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");

    assert!(
        stdout.contains("reconnect"),
        "expected 'reconnect' type: {stdout}"
    );
    assert!(
        stdout.contains("ready"),
        "expected 'ready' status: {stdout}"
    );
    assert!(
        stdout.contains(SAMPLE_SESSION_ID),
        "expected session_id in output: {stdout}"
    );
}

// ── Happy path with --cursor: JSON mode ──────────────────────────────────────

/// `synapse admin events reconnect-status --cursor <UUID> --json` exits 0 and
/// emits a valid JSON object.
#[test]
fn events_reconnect_status_with_cursor_happy_path_json() {
    let server = MockServer::spawn("happy");

    let output = synapse_cmd()
        .args([
            "--base-url",
            &server.base_url(),
            "admin",
            "events",
            "reconnect-status",
            "--cursor",
            SAMPLE_SESSION_ID,
            "--json",
        ])
        .output()
        .expect("command output");

    assert!(
        output.status.success(),
        "expected exit 0, got {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout should be valid JSON");

    assert!(parsed.is_object(), "expected JSON object: {stdout}");
    assert_eq!(parsed["type"], "reconnect");
    assert_eq!(parsed["status"]["status"], "ready");
    assert_eq!(parsed["status"]["session_id"], SAMPLE_SESSION_ID);
}

// ── Edge case: no --cursor is always valid ────────────────────────────────────

/// Calling `reconnect-status` without `--cursor` must exit 0 (not an error).
/// The server creates a fresh session and returns `status: ready`.
///
/// This is the documented "no active session" edge case: the command must
/// never fail when the caller omits the cursor.
#[test]
fn events_reconnect_status_no_cursor_is_not_an_error() {
    let server = MockServer::spawn("happy");

    let output = synapse_cmd()
        .args([
            "--base-url",
            &server.base_url(),
            "admin",
            "events",
            "reconnect-status",
            // intentionally no --cursor
        ])
        .output()
        .expect("command output");

    assert!(
        output.status.success(),
        "reconnect-status without cursor must exit 0, got {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(
        stdout.contains("ready"),
        "expected 'ready' status for fresh session: {stdout}"
    );
}

/// Same edge case in `--json` mode.
#[test]
fn events_reconnect_status_no_cursor_json_mode() {
    let server = MockServer::spawn("happy");

    let output = synapse_cmd()
        .args([
            "--base-url",
            &server.base_url(),
            "admin",
            "events",
            "reconnect-status",
            "--json",
        ])
        .output()
        .expect("command output");

    assert!(
        output.status.success(),
        "reconnect-status without cursor --json must exit 0, got {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout should be valid JSON");

    assert!(parsed.is_object(), "expected JSON object: {stdout}");
    assert_eq!(parsed["type"], "reconnect");
    // The server always returns ready for a fresh session.
    assert_eq!(parsed["status"]["status"], "ready");
    // backoff_seconds and requires_resync must be present.
    assert!(
        parsed["backoff_seconds"].is_number(),
        "backoff_seconds must be a number: {stdout}"
    );
    assert!(
        parsed["requires_resync"].is_boolean(),
        "requires_resync must be a boolean: {stdout}"
    );
}

// ── Edge case: session_expired returned by server is not an error ─────────────

/// When the server returns `status: session_expired` for a cursor lookup
/// (edge scenario), the command must still exit 0.
/// The expired-session state is a documented valid outcome, not a failure.
#[test]
fn events_reconnect_status_session_expired_is_not_an_error() {
    let server = MockServer::spawn("edge");

    let output = synapse_cmd()
        .args([
            "--base-url",
            &server.base_url(),
            "admin",
            "events",
            "reconnect-status",
            "--cursor",
            SAMPLE_SESSION_ID,
        ])
        .output()
        .expect("command output");

    assert!(
        output.status.success(),
        "session_expired must exit 0, got {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(
        stdout.contains("session_expired"),
        "expected 'session_expired' in output: {stdout}"
    );
}

/// JSON mode: session_expired via reconnect-status is exit 0, valid JSON.
#[test]
fn events_reconnect_status_session_expired_json_mode() {
    let server = MockServer::spawn("edge");

    let output = synapse_cmd()
        .args([
            "--base-url",
            &server.base_url(),
            "admin",
            "events",
            "reconnect-status",
            "--cursor",
            SAMPLE_SESSION_ID,
            "--json",
        ])
        .output()
        .expect("command output");

    assert!(
        output.status.success(),
        "session_expired --json must exit 0, got {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout should be valid JSON");

    assert_eq!(parsed["type"], "reconnect");
    assert_eq!(parsed["status"]["status"], "session_expired");
    assert_eq!(parsed["backoff_seconds"], 0);
    assert_eq!(parsed["requires_resync"], false);
}

// ── Server error propagates as non-zero exit ──────────────────────────────────

/// A genuine 5xx on `GET /reconnect/status` must cause the CLI to exit non-zero.
#[test]
fn events_reconnect_status_server_error_exits_nonzero() {
    let port = free_port();
    let listener = TcpListener::bind(format!("127.0.0.1:{port}")).expect("bind");

    let handle = thread::spawn(move || {
        use std::io::{BufRead, BufReader, Write};
        if let Ok((mut stream, _)) = listener.accept() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
            }
            let body = r#"{"error":"service unavailable"}"#;
            let resp = format!(
                "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(resp.as_bytes()).unwrap();
        }
    });

    wait_for_port(port);
    let base_url = format!("http://127.0.0.1:{port}");

    let output = synapse_cmd()
        .args([
            "--base-url",
            &base_url,
            "admin",
            "events",
            "reconnect-status",
        ])
        .output()
        .expect("command output");

    let _ = handle.join();

    assert!(
        !output.status.success(),
        "server 503 must exit non-zero, got {:?}",
        output.status
    );
}
