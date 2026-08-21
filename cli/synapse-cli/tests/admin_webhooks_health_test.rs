use assert_cmd::Command;
use std::net::TcpListener;
use std::process::{Child, Command as StdCommand, Stdio};
use std::thread;
use std::time::Duration;

const SAMPLE_ENDPOINT_ID: &str = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
const UNKNOWN_ENDPOINT_ID: &str = "00000000-0000-0000-0000-000000000000";

fn synapse_command() -> Command {
    Command::cargo_bin("synapse").expect("synapse binary exists")
}

// ── Mock server helpers (same pattern as cli.rs) ──────────────────────────────

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

// ── admin webhooks health (list) ──────────────────────────────────────────────

/// Happy path: table output lists endpoint ID, URL, success rate, and deliveries.
#[test]
fn webhooks_health_table_mode_happy_path() {
    let server = MockServer::spawn();

    let output = synapse_command()
        .args([
            "--base-url",
            &server.base_url(),
            "admin",
            "webhooks",
            "health",
        ])
        .output()
        .expect("command output");

    assert!(output.status.success(), "exit code must be 0");
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(stdout.contains(SAMPLE_ENDPOINT_ID));
    assert!(stdout.contains("https://example.com/webhook"));
    assert!(stdout.contains("99.5%"));
    assert!(stdout.contains("200"));
}

/// JSON flag: output is valid JSON array with expected fields.
#[test]
fn webhooks_health_json_mode() {
    let server = MockServer::spawn();

    let output = synapse_command()
        .args([
            "--base-url",
            &server.base_url(),
            "admin",
            "webhooks",
            "health",
            "--json",
        ])
        .output()
        .expect("command output");

    assert!(output.status.success(), "exit code must be 0");
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert!(parsed.is_array(), "response must be a JSON array");
    let first = &parsed[0];
    assert_eq!(first["id"], SAMPLE_ENDPOINT_ID);
    assert_eq!(first["url"], "https://example.com/webhook");
    assert_eq!(first["success_rate"], 99.5);
    assert_eq!(first["total_deliveries"], 200);
}

// ── admin webhooks health-get (single endpoint) ───────────────────────────────

/// Happy path: table output shows all fields for the requested endpoint.
#[test]
fn webhooks_health_get_table_mode_happy_path() {
    let server = MockServer::spawn();

    let output = synapse_command()
        .args([
            "--base-url",
            &server.base_url(),
            "admin",
            "webhooks",
            "health-get",
            SAMPLE_ENDPOINT_ID,
        ])
        .output()
        .expect("command output");

    assert!(output.status.success(), "exit code must be 0");
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(stdout.contains(SAMPLE_ENDPOINT_ID));
    assert!(stdout.contains("https://example.com/webhook"));
    assert!(stdout.contains("99.5%"));
    assert!(stdout.contains("200"));
}

/// JSON flag: output is valid JSON object with expected fields.
#[test]
fn webhooks_health_get_json_mode() {
    let server = MockServer::spawn();

    let output = synapse_command()
        .args([
            "--base-url",
            &server.base_url(),
            "admin",
            "webhooks",
            "health-get",
            SAMPLE_ENDPOINT_ID,
            "--json",
        ])
        .output()
        .expect("command output");

    assert!(output.status.success(), "exit code must be 0");
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(parsed["id"], SAMPLE_ENDPOINT_ID);
    assert_eq!(parsed["url"], "https://example.com/webhook");
    assert_eq!(parsed["success_rate"], 99.5);
    assert_eq!(parsed["total_deliveries"], 200);
    assert_eq!(parsed["enabled"], true);
}

/// Edge case: a non-existent endpoint ID returns exit code 1 with an error message.
#[test]
fn webhooks_health_get_unknown_id_returns_nonzero_exit() {
    let server = MockServer::spawn();

    let output = synapse_command()
        .args([
            "--base-url",
            &server.base_url(),
            "admin",
            "webhooks",
            "health-get",
            UNKNOWN_ENDPOINT_ID,
        ])
        .output()
        .expect("command output");

    assert!(
        !output.status.success(),
        "exit code must be non-zero for 404"
    );
    let stderr = String::from_utf8(output.stderr).expect("valid utf-8");
    assert!(
        stderr.contains("404") || stderr.contains("not found") || stderr.contains("error"),
        "stderr must mention the failure: {stderr}",
    );
}
