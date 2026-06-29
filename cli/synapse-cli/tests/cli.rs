use assert_cmd::Command;
use mockito::Server;
use serde_json::json;
use std::net::TcpListener;
use std::process::{Child, Command as StdCommand, Stdio};
use std::thread;
use std::time::Duration;

const SAMPLE_REPORT_ID: &str = "3f1d8c31-5f1d-4fb8-93e0-112233445566";

#[test]
fn reconciliation_commands_table_mode_happy_path() {
    let server = MockServer::spawn("happy");
    let base_url = server.base_url();

    let mut cmd = synapse_command();
    cmd.args(["--base-url", &base_url, "admin", "reconciliation", "reports"]);

    let output = cmd.output().expect("reports output");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(stdout.contains("Reports: 1 total"));
    assert!(stdout.contains("ID | Generated | Period Start"));
    assert!(stdout.contains(SAMPLE_REPORT_ID));
    assert!(stdout.contains("| yes"));

    let mut cmd = synapse_command();
    cmd.args([
        "--base-url",
        &base_url,
        "admin",
        "reconciliation",
        "report",
        SAMPLE_REPORT_ID,
    ]);

    let output = cmd.output().expect("report output");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(stdout.contains("Report ID:"));
    assert!(stdout.contains("Summary:"));
    assert!(stdout.contains("Has discrepancies: yes"));

    let mut cmd = synapse_command();
    cmd.args([
        "--base-url",
        &base_url,
        "admin",
        "reconciliation",
        "run",
        "--account",
        "GA_TEST_ACCOUNT",
    ]);

    let output = cmd.output().expect("run output");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(stdout.contains("Reconciliation completed successfully"));
    assert!(stdout.contains("Database transactions: 12"));
    assert!(stdout.contains("Has discrepancies: yes"));
}

#[test]
fn run_help_text_mentions_required_and_optional_flags() {
    let mut cmd = Command::cargo_bin("synapse").expect("binary exists");
    cmd.args(["admin", "reconciliation", "run", "--help"]);
    let output = cmd.output().expect("help output");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");

    assert!(stdout.contains("Required flags:"));
    assert!(stdout.contains("--account <ACCOUNT>"));
    assert!(stdout.contains("Optional flags:"));
    assert!(stdout.contains("--period-hours <HOURS>"));
}

#[tokio::test]
async fn run_command_prints_mock_server_summary() {
    let mut server = Server::new_async().await;
    let _mock = server
        .mock("POST", "/admin/reconciliation/run")
        .match_header("content-type", "application/json")
        .match_body(r#"{"account":"GA_TEST_ACCOUNT","period_hours":24}"#)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            json!({
                "message": "Reconciliation completed successfully",
                "report": {
                    "id": "3f1d8c31-5f1d-4fb8-93e0-112233445566",
                    "generated_at": "2026-06-27T06:10:12Z",
                    "period_start": "2026-06-26T06:10:12Z",
                    "period_end": "2026-06-27T06:10:12Z",
                    "total_db_transactions": 12,
                    "total_chain_payments": 11,
                    "missing_on_chain_count": 1,
                    "orphaned_payments_count": 0,
                    "amount_mismatches_count": 1,
                    "has_discrepancies": true
                }
            })
            .to_string(),
        )
        .create_async()
        .await;

    let mut cmd = Command::cargo_bin("synapse").expect("binary exists");
    cmd.args([
        "--base-url",
        &server.url(),
        "admin",
        "reconciliation",
        "run",
        "--account",
        "GA_TEST_ACCOUNT",
    ]);

    let output = cmd.output().expect("run output");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(stdout.contains("Reconciliation completed successfully"));
    assert!(stdout.contains("Database transactions: 12"));
    assert!(stdout.contains("Has discrepancies: yes"));
}

#[test]
fn admin_bulk_status_table_and_json_output_happy_path() {
    let server = MockServer::spawn("happy");
    let base_url = server.base_url();

    let mut cmd = synapse_command();
    cmd.args([
        "--base-url",
        &base_url,
        "admin",
        "transactions",
        "bulk-status",
        "--ids",
        "550e8400-e29b-41d4-a716-446655440000,550e8400-e29b-41d4-a716-446655440001",
        "--status",
        "completed",
    ]);

    let output = cmd.output().expect("bulk status output");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(stdout.contains("updated: 2"));
    assert!(stdout.contains("failed: 0"));

    let mut cmd = synapse_command();
    cmd.args([
        "--base-url",
        &base_url,
        "admin",
        "transactions",
        "bulk-status",
        "--ids",
        "550e8400-e29b-41d4-a716-446655440000,550e8400-e29b-41d4-a716-446655440001",
        "--status",
        "completed",
        "--format",
        "json",
    ]);

    let output = cmd.output().expect("bulk status json output");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(stdout.contains("\"updated\": 2"));
    assert!(stdout.contains("\"failed\": 0"));
    assert!(stdout.contains("\"errors\": []"));
}

#[test]
fn admin_bulk_status_partial_failure_edge_case_is_reported() {
    let server = MockServer::spawn("edge");
    let base_url = server.base_url();

    let mut cmd = synapse_command();
    cmd.args([
        "--base-url",
        &base_url,
        "admin",
        "transactions",
        "bulk-status",
        "--ids",
        "550e8400-e29b-41d4-a716-446655440000,550e8400-e29b-41d4-a716-446655440001",
        "--status",
        "failed",
        "--format",
        "json",
    ]);

    let output = cmd.output().expect("bulk status edge output");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(stdout.contains("\"updated\": 1"));
    assert!(stdout.contains("\"failed\": 1"));
    assert!(stdout.contains("\"transaction_id\": \"550e8400-e29b-41d4-a716-446655440001\""));
    assert!(stdout.contains("\"error\": \"status transition not allowed\""));
}

#[test]
fn reconciliation_commands_json_mode_edge_case() {
    let server = MockServer::spawn("edge");
    let base_url = server.base_url();

    let mut cmd = synapse_command();
    cmd.args([
        "--base-url",
        &base_url,
        "admin",
        "reconciliation",
        "reports",
        "--json",
    ]);

    let output = cmd.output().expect("reports json output");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(stdout.contains("\"total\": 0"));
    assert!(stdout.contains("\"reports\": []"));

    let mut cmd = synapse_command();
    cmd.args([
        "--base-url",
        &base_url,
        "admin",
        "reconciliation",
        "report",
        SAMPLE_REPORT_ID,
    ]);

    let output = cmd.output().expect("report output");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(stdout.contains("No discrepancies found"));
    assert!(stdout.contains("Has discrepancies: no"));

    let mut cmd = synapse_command();
    cmd.args([
        "--base-url",
        &base_url,
        "admin",
        "reconciliation",
        "run",
        "--account",
        "GA_TEST_ACCOUNT",
        "--json",
    ]);

    let output = cmd.output().expect("run json output");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(stdout.contains("\"message\": \"Reconciliation completed successfully\""));
    assert!(stdout.contains("\"has_discrepancies\": false"));
    assert!(stdout.contains("\"total_db_transactions\": 0"));
}

fn synapse_command() -> Command {
    Command::cargo_bin("synapse").expect("synapse binary exists")
}

struct MockServer {
    child: Child,
    port: u16,
}

impl MockServer {
    fn spawn(scenario: &str) -> Self {
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

impl Drop for MockServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
