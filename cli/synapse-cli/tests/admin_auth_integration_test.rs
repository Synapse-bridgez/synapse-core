//! Integration coverage for issue #1065:
//!
//!   - Part A: every admin-class command (`admin ...`, `stats ...`,
//!     `admin webhooks health*`) now authenticates with `Authorization:
//!     Bearer <token>` against a mock server that actually validates it —
//!     success with the correct token, `401`/exit 2 with an incorrect one.
//!   - Part B: `transactions export` hits the correct `/export` path with
//!     admin auth, and `graphql query` sends admin auth too.
//!   - Part C: the real compiled binary's process exit code differentiates
//!     an auth failure (2) from a not-found (3) from any other error (1) —
//!     not the unconditional `exit(1)` every failure used to produce.
//!
//! Unlike the mockito/wiremock-based unit tests elsewhere in this crate
//! (which never send credentials and never got auth wrong to catch it),
//! these tests run the real `synapse` and `mock-server` binaries as
//! subprocesses over a real TCP connection, so a wrong header name is
//! actually observable here the way it wasn't before this fix — mock-server
//! now validates `Authorization` the same way the real server's
//! `admin_auth` middleware does.

use assert_cmd::Command;
use std::net::TcpListener;
use std::process::{Child, Command as StdCommand, Stdio};
use std::thread;
use std::time::Duration;

const SAMPLE_ENDPOINT_ID: &str = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
const UNKNOWN_ENDPOINT_ID: &str = "00000000-0000-0000-0000-000000000000";

/// Matches `mock-server.rs`'s `DEFAULT_ADMIN_TOKEN`.
const CORRECT_TOKEN: &str = "test-admin-token";
const WRONG_TOKEN: &str = "not-the-right-token";

fn synapse_command() -> Command {
    Command::cargo_bin("synapse").expect("synapse binary exists")
}

// ── Mock server helper (same pattern as cli.rs / admin_webhooks_health_test.rs) ─

struct MockServer {
    child: Child,
    port: u16,
}

impl MockServer {
    fn spawn() -> Self {
        let port = free_port();
        let binary =
            std::env::var_os("CARGO_BIN_EXE_mock-server").expect("mock-server binary path");
        let child = StdCommand::new(binary)
            .env("MOCK_SERVER_ADDR", format!("127.0.0.1:{port}"))
            .env("MOCK_SERVER_SCENARIO", "happy")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("mock server to start");

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

// ── Part A: admin-class commands authenticate with Authorization: Bearer ────

/// One representative command per auth class named in the issue (admin,
/// stats, webhooks-as-admin) succeeds with the correct token and fails
/// with `401`/exit 2 with an incorrect one.
#[test]
fn admin_locks_list_succeeds_with_correct_token_fails_with_wrong_token() {
    let server = MockServer::spawn();

    let ok = synapse_command()
        .args([
            "--base-url",
            &server.base_url(),
            "--api-key",
            CORRECT_TOKEN,
            "admin",
            "locks",
            "list",
        ])
        .output()
        .expect("command output");
    assert!(
        ok.status.success(),
        "expected success with correct token, stderr: {}",
        String::from_utf8_lossy(&ok.stderr)
    );

    let bad = synapse_command()
        .args([
            "--base-url",
            &server.base_url(),
            "--api-key",
            WRONG_TOKEN,
            "admin",
            "locks",
            "list",
        ])
        .output()
        .expect("command output");
    assert_eq!(
        bad.status.code(),
        Some(2),
        "expected EXIT_AUTH_FAILURE (2) for a wrong admin token, got {:?}\nstderr: {}",
        bad.status.code(),
        String::from_utf8_lossy(&bad.stderr)
    );

    let missing = synapse_command()
        .args(["--base-url", &server.base_url(), "admin", "locks", "list"])
        .output()
        .expect("command output");
    assert_eq!(
        missing.status.code(),
        Some(2),
        "expected EXIT_AUTH_FAILURE (2) with no token at all"
    );
}

#[test]
fn stats_status_succeeds_with_correct_token_fails_with_wrong_token() {
    let server = MockServer::spawn();

    let ok = synapse_command()
        .args([
            "--base-url",
            &server.base_url(),
            "--api-key",
            CORRECT_TOKEN,
            "stats",
            "status",
        ])
        .output()
        .expect("command output");
    assert!(ok.status.success());

    let bad = synapse_command()
        .args([
            "--base-url",
            &server.base_url(),
            "--api-key",
            WRONG_TOKEN,
            "stats",
            "status",
        ])
        .output()
        .expect("command output");
    assert_eq!(bad.status.code(), Some(2));
}

#[test]
fn admin_webhooks_health_succeeds_with_correct_token_fails_with_wrong_token() {
    let server = MockServer::spawn();

    let ok = synapse_command()
        .args([
            "--base-url",
            &server.base_url(),
            "--api-key",
            CORRECT_TOKEN,
            "admin",
            "webhooks",
            "health",
        ])
        .output()
        .expect("command output");
    assert!(ok.status.success());

    let bad = synapse_command()
        .args([
            "--base-url",
            &server.base_url(),
            "--api-key",
            WRONG_TOKEN,
            "admin",
            "webhooks",
            "health",
        ])
        .output()
        .expect("command output");
    assert_eq!(bad.status.code(), Some(2));
}

// ── Part B: transactions export and graphql query ────────────────────────────

#[test]
fn transactions_export_hits_export_path_with_admin_auth() {
    let server = MockServer::spawn();

    let ok = synapse_command()
        .args([
            "--base-url",
            &server.base_url(),
            "--api-key",
            CORRECT_TOKEN,
            "transactions",
            "export",
            "--format",
            "csv",
        ])
        .output()
        .expect("command output");
    assert!(
        ok.status.success(),
        "expected success with correct token, stderr: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let stdout = String::from_utf8(ok.stdout).expect("valid utf-8");
    assert!(
        stdout.starts_with("id,stellar_account,amount"),
        "expected CSV export output, got: {stdout}"
    );

    let bad = synapse_command()
        .args([
            "--base-url",
            &server.base_url(),
            "--api-key",
            WRONG_TOKEN,
            "transactions",
            "export",
        ])
        .output()
        .expect("command output");
    assert_eq!(
        bad.status.code(),
        Some(2),
        "expected EXIT_AUTH_FAILURE (2) for a wrong admin token on /export"
    );
}

#[test]
fn graphql_query_sends_admin_auth() {
    let server = MockServer::spawn();

    let ok = synapse_command()
        .args([
            "--base-url",
            &server.base_url(),
            "--api-key",
            CORRECT_TOKEN,
            "graphql",
            "query",
            "--query",
            "{ transactions { id status } }",
        ])
        .output()
        .expect("command output");
    assert!(
        ok.status.success(),
        "expected success with correct token, stderr: {}",
        String::from_utf8_lossy(&ok.stderr)
    );

    let bad = synapse_command()
        .args([
            "--base-url",
            &server.base_url(),
            "--api-key",
            WRONG_TOKEN,
            "graphql",
            "query",
            "--query",
            "{ transactions { id status } }",
        ])
        .output()
        .expect("command output");
    assert_eq!(
        bad.status.code(),
        Some(2),
        "expected EXIT_AUTH_FAILURE (2) for a wrong admin token on /graphql"
    );
}

// ── Part C: real process exit codes ───────────────────────────────────────────

/// The real compiled binary — not just the `handle_error`/`CliError` unit
/// tests in `tests/error_handling.rs`, which only ever exercised
/// hand-constructed `CliError` values, never main.rs's actual error path —
/// exits 2 for an auth failure.
#[test]
fn real_binary_exits_2_on_auth_failure() {
    let server = MockServer::spawn();

    let output = synapse_command()
        .args([
            "--base-url",
            &server.base_url(),
            "--api-key",
            WRONG_TOKEN,
            "admin",
            "locks",
            "list",
        ])
        .output()
        .expect("command output");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).expect("valid utf-8");
    assert!(
        stderr.starts_with("error:"),
        "stderr should start with 'error:', got: {stderr}"
    );
}

/// The real compiled binary exits 3 for a not-found case.
#[test]
fn real_binary_exits_3_on_not_found() {
    let server = MockServer::spawn();

    let output = synapse_command()
        .args([
            "--base-url",
            &server.base_url(),
            "--api-key",
            CORRECT_TOKEN,
            "admin",
            "webhooks",
            "health-get",
            UNKNOWN_ENDPOINT_ID,
        ])
        .output()
        .expect("command output");

    assert_eq!(output.status.code(), Some(3));
}

/// The real compiled binary exits 0 on success (sanity check that the new
/// exit-code plumbing didn't regress the happy path).
#[test]
fn real_binary_exits_0_on_success() {
    let server = MockServer::spawn();

    let output = synapse_command()
        .args([
            "--base-url",
            &server.base_url(),
            "--api-key",
            CORRECT_TOKEN,
            "admin",
            "webhooks",
            "health-get",
            SAMPLE_ENDPOINT_ID,
        ])
        .output()
        .expect("command output");

    assert_eq!(output.status.code(), Some(0));
}
