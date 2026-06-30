use assert_cmd::Command;
use std::net::TcpListener;
use std::process::{Child, Command as StdCommand, Stdio};
use std::thread;
use std::time::Duration;

fn synapse_command() -> Command {
    Command::cargo_bin("synapse").expect("synapse binary exists")
}

struct MockServer {
    child: Child,
    port: u16,
}

impl MockServer {
    fn spawn_scenario(scenario: &str) -> Self {
        let port = free_port();
        let binary = std::env::var_os("CARGO_BIN_EXE_mock-server")
            .expect("mock-server binary path");
        let child = StdCommand::new(binary)
            .env("MOCK_SERVER_ADDR", format!("127.0.0.1:{port}"))
            .env("MOCK_SERVER_SCENARIO", scenario)
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

/// Happy path: table output shows lock resource, token, and summary counts.
#[test]
fn locks_list_table_happy_path() {
    let server = MockServer::spawn_scenario("happy");

    let output = synapse_command()
        .args(["--base-url", &server.base_url(), "admin", "locks", "list"])
        .output()
        .expect("command output");

    assert!(output.status.success(), "exit code must be 0");
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(stdout.contains("settlement:42"), "must show lock resource");
    assert!(stdout.contains("tok-abc123"), "must show lock token");
    assert!(stdout.contains('1'), "must show total count");
}

/// JSON flag: output is valid JSON with expected fields.
#[test]
fn locks_list_json_mode() {
    let server = MockServer::spawn_scenario("happy");

    let output = synapse_command()
        .args([
            "--base-url",
            &server.base_url(),
            "admin",
            "locks",
            "list",
            "--json",
        ])
        .output()
        .expect("command output");

    assert!(output.status.success(), "exit code must be 0");
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(parsed["total"], 1);
    assert_eq!(parsed["overdue"], 0);
    let locks = parsed["active_locks"].as_array().expect("active_locks is array");
    assert_eq!(locks.len(), 1);
    assert_eq!(locks[0]["resource"], "settlement:42");
    assert_eq!(locks[0]["token"], "tok-abc123");
    assert_eq!(locks[0]["overdue"], false);
}

/// Edge case: empty active_locks list (not null) is handled gracefully.
#[test]
fn locks_list_empty_list_not_null() {
    let server = MockServer::spawn_scenario("edge");

    let output = synapse_command()
        .args(["--base-url", &server.base_url(), "admin", "locks", "list"])
        .output()
        .expect("command output");

    assert!(output.status.success(), "exit code must be 0 for empty list");
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(
        stdout.contains("No locks currently held."),
        "must show empty-list message: {stdout}",
    );
}

/// Edge case: empty list in JSON mode returns valid JSON with empty array, not null.
#[test]
fn locks_list_empty_list_json_mode() {
    let server = MockServer::spawn_scenario("edge");

    let output = synapse_command()
        .args([
            "--base-url",
            &server.base_url(),
            "admin",
            "locks",
            "list",
            "--json",
        ])
        .output()
        .expect("command output");

    assert!(output.status.success(), "exit code must be 0");
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(parsed["total"], 0);
    assert!(
        parsed["active_locks"].is_array(),
        "active_locks must be an array, not null",
    );
    assert_eq!(
        parsed["active_locks"].as_array().unwrap().len(),
        0,
        "active_locks must be empty array",
    );
}
