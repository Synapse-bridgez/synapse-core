/// Integration tests for `synapse admin events watch`.
///
/// These tests spin up the mock HTTP server binary (Issue #64) and run the
/// compiled `synapse` binary (Issue #89) as a subprocess, asserting on the
/// process exit code and on the content printed to stdout in both table mode
/// (default) and `--json` mode.
use assert_cmd::Command;
use std::net::TcpListener;
use std::process::{Child, Command as StdCommand, Stdio};
use std::thread;
use std::time::Duration;

// ── Mock server harness ───────────────────────────────────────────────────────

struct MockServer {
    child: Child,
    port: u16,
}

impl MockServer {
    fn spawn() -> Self {
        let port = free_port();
        let binary = std::env::var_os("CARGO_BIN_EXE_mock-server")
            .expect("CARGO_BIN_EXE_mock-server must be set; run via `cargo test`");
        let child = StdCommand::new(binary)
            .env("MOCK_SERVER_ADDR", format!("127.0.0.1:{port}"))
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

// ── Helpers ───────────────────────────────────────────────────────────────────

fn synapse_cmd() -> Command {
    Command::cargo_bin("synapse").expect("synapse binary must exist")
}

// ── Happy-path: table mode ────────────────────────────────────────────────────

/// `synapse admin events watch` exits 0 and prints a table with the two
/// canned events served by the mock server.
#[test]
fn events_watch_happy_path_table() {
    let server = MockServer::spawn();

    let output = synapse_cmd()
        .args(["--base-url", &server.base_url(), "admin", "events", "watch"])
        .output()
        .expect("command output");

    assert!(
        output.status.success(),
        "expected exit 0, got {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");

    // Header row
    assert!(
        stdout.contains("TRANSACTION ID"),
        "expected table header in: {stdout}"
    );
    // First event
    assert!(
        stdout.contains("aaaaaaaa-0000-0000-0000-000000000001"),
        "expected first transaction_id in: {stdout}"
    );
    assert!(stdout.contains("pending"), "expected status 'pending' in: {stdout}");
    // Second event
    assert!(
        stdout.contains("aaaaaaaa-0000-0000-0000-000000000002"),
        "expected second transaction_id in: {stdout}"
    );
    assert!(stdout.contains("completed"), "expected status 'completed' in: {stdout}");
}

// ── Happy-path: JSON mode ─────────────────────────────────────────────────────

/// `synapse admin events watch --json` exits 0 and emits a JSON array
/// containing the two canned events.
#[test]
fn events_watch_happy_path_json() {
    let server = MockServer::spawn();

    let output = synapse_cmd()
        .args([
            "--base-url",
            &server.base_url(),
            "admin",
            "events",
            "watch",
            "--json",
        ])
        .output()
        .expect("command output");

    assert!(
        output.status.success(),
        "expected exit 0, got {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");

    // Must be parseable as a JSON array
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout should be valid JSON");
    assert!(parsed.is_array(), "expected JSON array, got: {stdout}");

    let arr = parsed.as_array().unwrap();
    assert_eq!(arr.len(), 2, "expected 2 events in JSON output");

    assert_eq!(
        arr[0]["transaction_id"],
        "aaaaaaaa-0000-0000-0000-000000000001"
    );
    assert_eq!(arr[0]["status"], "pending");
    assert_eq!(
        arr[1]["transaction_id"],
        "aaaaaaaa-0000-0000-0000-000000000002"
    );
    assert_eq!(arr[1]["status"], "completed");
    assert_eq!(arr[1]["message"], "settled");
}

// ── Edge case: empty event list ───────────────────────────────────────────────

/// When the server returns an empty `[]`, the command exits 0 (not an error)
/// and prints the "No events" sentinel rather than a table with rows.
///
/// This covers the documented edge case: an empty event stream is valid.
#[test]
fn events_watch_empty_list_is_not_an_error() {
    // The mock server serves [] for the path /events?empty=1.
    // We point the CLI at a minimal server that always returns [].
    let port = free_port();

    // Spawn a second mock-server instance; we'll send a normal /events request
    // but rely on the fact that we can verify "No events" by hitting a custom
    // tiny server below.  Because we cannot easily reconfigure the mock binary
    // per-request, we instead spawn a minimal TCP server inline.
    let listener = TcpListener::bind(format!("127.0.0.1:{port}")).expect("bind");
    // Spawn a thread that serves the first real HTTP request with an empty
    // JSON array. `wait_for_port`'s readiness probe below also opens (and
    // immediately drops) a connection, which would otherwise race for this
    // listener's single `accept()`; skip over any such empty connections and
    // wait for one that actually carries a request line.
    let handle = thread::spawn(move || {
        use std::io::{BufRead, BufReader, Write};
        loop {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first_line = String::new();
            if reader.read_line(&mut first_line).unwrap_or(0) == 0 || first_line.trim().is_empty() {
                continue;
            }
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
            }
            let body = "[]";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
            break;
        }
    });

    wait_for_port(port);
    let base_url = format!("http://127.0.0.1:{port}");

    let output = synapse_cmd()
        .args(["--base-url", &base_url, "admin", "events", "watch"])
        .output()
        .expect("command output");

    let _ = handle.join();

    assert!(
        output.status.success(),
        "empty event list must exit 0, got {:?}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(
        stdout.contains("No events"),
        "expected 'No events' sentinel for empty list, got: {stdout}"
    );
}

/// Same edge case in `--json` mode: empty array is printed as `[]`, exit 0.
#[test]
fn events_watch_empty_list_json_mode() {
    let port = free_port();
    let listener = TcpListener::bind(format!("127.0.0.1:{port}")).expect("bind");

    let handle = thread::spawn(move || {
        use std::io::{BufRead, BufReader, Write};
        loop {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first_line = String::new();
            if reader.read_line(&mut first_line).unwrap_or(0) == 0 || first_line.trim().is_empty() {
                continue;
            }
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
            }
            let body = "[]";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
            break;
        }
    });

    wait_for_port(port);
    let base_url = format!("http://127.0.0.1:{port}");

    let output = synapse_cmd()
        .args(["--base-url", &base_url, "admin", "events", "watch", "--json"])
        .output()
        .expect("command output");

    let _ = handle.join();

    assert!(
        output.status.success(),
        "empty event list --json must exit 0, got {:?}",
        output.status
    );

    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("stdout should be valid JSON");
    assert!(
        parsed.as_array().map(|a| a.is_empty()).unwrap_or(false),
        "expected empty JSON array, got: {stdout}"
    );
}
