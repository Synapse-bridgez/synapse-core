use assert_cmd::Command;
use mockito::{Matcher, Server};
use serde_json::json;
use std::net::TcpListener;
use std::process::{Child, Stdio};
use std::thread;
use std::time::Duration;

const SAMPLE_REPORT_ID: &str = "3f1d8c31-5f1d-4fb8-93e0-112233445566";
const SAMPLE_LOCK_TOKEN: &str = "4e4e9e47-7e0f-4f2f-8d63-323c61279209";
const TENANT_ID: &str = "550e8400-e29b-41d4-a716-446655440000";
const SETTLEMENT_ID: &str = "660e8400-e29b-41d4-a716-446655440000";

#[test]
fn reconciliation_commands_table_mode_happy_path() {
    let server = MockServer::spawn("happy");
    let base_url = server.base_url();

    let output = synapse_command()
        .args([
            "--base-url",
            &base_url,
            "admin",
            "reconciliation",
            "reports",
        ])
        .output()
        .expect("reports output");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(stdout.contains("Reports: 1 total"));
    assert!(stdout.contains("ID | Generated | Period Start"));
    assert!(stdout.contains(SAMPLE_REPORT_ID));
    assert!(stdout.contains("| yes"));

    let output = synapse_command()
        .args([
            "--base-url",
            &base_url,
            "admin",
            "reconciliation",
            "report",
            SAMPLE_REPORT_ID,
        ])
        .output()
        .expect("report output");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(stdout.contains("Report ID:"));
    assert!(stdout.contains("Summary:"));
    assert!(stdout.contains("Has discrepancies: yes"));
}

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
#[test]
fn reconciliation_commands_json_mode_edge_case() {
    let server = MockServer::spawn("edge");
    let base_url = server.base_url();

    let output = synapse_command()
        .args([
            "--base-url",
            &base_url,
            "admin",
            "reconciliation",
            "reports",
            "--json",
        ])
        .output()
        .expect("reports json output");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(stdout.contains("\"total\": 0"));
    assert!(stdout.contains("\"reports\": []"));

    let output = synapse_command()
        .args([
            "--base-url",
            &base_url,
            "admin",
            "reconciliation",
            "report",
            SAMPLE_REPORT_ID,
        ])
        .output()
        .expect("report output");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(stdout.contains("No discrepancies found"));
    assert!(stdout.contains("Has discrepancies: no"));
}

#[test]
fn locks_list_table_mode_happy_path() {
    let server = MockServer::spawn("happy");
    let base_url = server.base_url();

    let output = synapse_command()
        .args(["--base-url", &base_url, "admin", "locks", "list"])
        .output()
        .expect("locks list output");

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(stdout.contains("Active locks: 2 total (1 overdue)"));
    assert!(stdout.contains("Resource | Token | Acquired At | TTL | Expected Duration | Overdue"));
    assert!(stdout.contains("settlement:550e8400-e29b-41d4-a716-446655440000"));
    assert!(stdout.contains(SAMPLE_LOCK_TOKEN));
    assert!(stdout.contains("payout-batch:daily"));
    assert!(stdout.contains("| yes"));
}

#[test]
fn locks_list_json_mode_happy_path() {
    let server = MockServer::spawn("happy");
    let base_url = server.base_url();

    let output = synapse_command()
        .args(["--base-url", &base_url, "admin", "locks", "list", "--json"])
        .output()
        .expect("locks list json output");

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(stdout.contains("\"active_locks\": ["));
    assert!(stdout.contains("\"resource\": \"settlement:550e8400-e29b-41d4-a716-446655440000\""));
    assert!(stdout.contains(&format!("\"token\": \"{SAMPLE_LOCK_TOKEN}\"")));
    assert!(stdout.contains("\"total\": 2"));
    assert!(stdout.contains("\"overdue\": 1"));
}

#[test]
fn locks_list_handles_empty_response_edge_case() {
    let server = MockServer::spawn("edge");
    let base_url = server.base_url();

    let output = synapse_command()
        .args(["--base-url", &base_url, "admin", "locks", "list"])
        .output()
        .expect("locks list empty output");

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(stdout.contains("Active locks: 0 total (0 overdue)"));
    assert!(stdout.contains("No active locks found"));

    let output = synapse_command()
        .args(["--base-url", &base_url, "admin", "locks", "list", "--json"])
        .output()
        .expect("locks list empty json output");

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(stdout.contains("\"active_locks\": []"));
    assert!(stdout.contains("\"total\": 0"));
    assert!(stdout.contains("\"overdue\": 0"));
}

#[test]
fn locks_list_help_text_mentions_required_and_optional_flags() {
    let output = synapse_command()
        .args(["admin", "locks", "list", "--help"])
        .output()
        .expect("locks help output");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");

    assert!(stdout.contains("Required flags: none"));
    assert!(stdout.contains("Optional flags:"));
    assert!(stdout.contains("--json"));
    assert!(stdout.contains("Output fields:"));
    assert!(stdout.contains("resource"));
    assert!(stdout.contains("overdue"));
}

#[test]
fn run_help_text_mentions_required_and_optional_flags() {
    let output = synapse_command()
        .args(["admin", "reconciliation", "run", "--help"])
        .output()
        .expect("help output");
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
                    "id": SAMPLE_REPORT_ID,
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

    let output = synapse_command()
        .args([
            "--base-url",
            &server.url(),
            "admin",
            "reconciliation",
            "run",
            "--account",
            "GA_TEST_ACCOUNT",
        ])
        .output()
        .expect("run output");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(stdout.contains("Reconciliation completed successfully"));
    assert!(stdout.contains("Database transactions: 12"));
    assert!(stdout.contains("Has discrepancies: yes"));
}

#[tokio::test]
async fn quotas_list_get_set_and_reset_use_formatter_output() {
    let mut server = Server::new_async().await;
    let quota = json!({
        "tenant_id": TENANT_ID,
        "name": "tenant-a",
        "rate_limit_per_minute": 25,
        "quota_status": {
            "limit": 25,
            "remaining": 20,
            "reset_at": "2026-06-27T06:10:12Z"
        }
    });

    let _list = server
        .mock("GET", "/admin/quotas")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(json!([quota.clone()]).to_string())
        .create_async()
        .await;
    let output = synapse_command()
        .args(["--base-url", &server.url(), "admin", "quotas", "list"])
        .output()
        .expect("quotas list output");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(stdout.contains("tenant-a"));
    assert!(stdout.contains("25"));
    assert!(stdout.contains(TENANT_ID));

    let get_path = format!("/admin/quotas/{TENANT_ID}");
    let _get = server
        .mock("GET", get_path.as_str())
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(quota.to_string())
        .create_async()
        .await;
    let output = synapse_command()
        .args([
            "--base-url",
            &server.url(),
            "admin",
            "quotas",
            "get",
            TENANT_ID,
            "--json",
        ])
        .output()
        .expect("quotas get output");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(stdout.contains("\"tenant_id\": \"550e8400-e29b-41d4-a716-446655440000\""));

    let set_path = format!("/admin/quotas/{TENANT_ID}");
    let _set = server
        .mock("PUT", set_path.as_str())
        .match_header("content-type", "application/json")
        .match_body(r#"{"custom_limit":50}"#)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(json!({"message": "quota updated", "tenant_id": TENANT_ID}).to_string())
        .create_async()
        .await;
    let output = synapse_command()
        .args([
            "--base-url",
            &server.url(),
            "admin",
            "quotas",
            "set",
            TENANT_ID,
            "--limit",
            "50",
        ])
        .output()
        .expect("quotas set output");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(stdout.contains("message: quota updated"));

    let reset_path = format!("/admin/quotas/{TENANT_ID}/reset");
    let _reset = server
        .mock("DELETE", reset_path.as_str())
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(json!({"message": "quota reset", "tenant_id": TENANT_ID}).to_string())
        .create_async()
        .await;
    let output = synapse_command()
        .args([
            "--base-url",
            &server.url(),
            "admin",
            "quotas",
            "reset",
            TENANT_ID,
            "--json",
        ])
        .output()
        .expect("quotas reset output");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(stdout.contains("\"message\": \"quota reset\""));
}

#[tokio::test]
async fn admin_settlements_update_status_uses_formatter_output() {
    let mut server = Server::new_async().await;
    let path = format!("/admin/settlements/{SETTLEMENT_ID}/status");
    let settlement = json!({
        "id": SETTLEMENT_ID,
        "status": "pending_review",
        "amount": "1500.00",
        "asset_code": "USD",
        "updated_at": "2026-06-27T06:10:12Z"
    });

    let _update = server
        .mock("PATCH", path.as_str())
        .match_header("content-type", "application/json")
        .match_body(Matcher::PartialJson(json!({
            "status": "pending_review",
            "reason": "manual review",
            "actor": "ops"
        })))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(settlement.to_string())
        .create_async()
        .await;

    let output = synapse_command()
        .args([
            "--base-url",
            &server.url(),
            "admin",
            "settlements",
            "update-status",
            SETTLEMENT_ID,
            "pending_review",
            "--reason",
            "manual review",
            "--actor",
            "ops",
        ])
        .output()
        .expect("admin settlement update-status output");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(stdout.contains(&format!("id: {SETTLEMENT_ID}")));
    assert!(stdout.contains("status: pending_review"));
    assert!(stdout.contains("amount: 1500.00"));
}

#[tokio::test]
async fn admin_settlements_update_status_supports_json_output() {
    let mut server = Server::new_async().await;
    let path = format!("/admin/settlements/{SETTLEMENT_ID}/status");

    let _update = server
        .mock("PATCH", path.as_str())
        .match_body(Matcher::PartialJson(json!({
            "status": "disputed"
        })))
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            json!({
                "id": SETTLEMENT_ID,
                "status": "disputed",
                "amount": "1500.00",
                "asset_code": "USD"
            })
            .to_string(),
        )
        .create_async()
        .await;

    let output = synapse_command()
        .args([
            "--base-url",
            &server.url(),
            "admin",
            "settlements",
            "update-status",
            SETTLEMENT_ID,
            "disputed",
            "--json",
        ])
        .output()
        .expect("admin settlement update-status json output");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(stdout.contains(&format!("\"id\": \"{SETTLEMENT_ID}\"")));
    assert!(stdout.contains("\"status\": \"disputed\""));
}

#[tokio::test]
async fn admin_settlements_update_status_surfaces_invalid_transition_message() {
    let mut server = Server::new_async().await;
    let path = format!("/admin/settlements/{SETTLEMENT_ID}/status");

    let _update = server
        .mock("PATCH", path.as_str())
        .match_body(Matcher::PartialJson(json!({
            "status": "pending"
        })))
        .with_status(400)
        .with_header("content-type", "application/json")
        .with_body(
            json!({
                "error": "Bad request: invalid transition: completed -> pending",
                "code": "BAD_REQUEST_001",
                "status": 400
            })
            .to_string(),
        )
        .create_async()
        .await;

    let output = synapse_command()
        .args([
            "--base-url",
            &server.url(),
            "admin",
            "settlements",
            "update-status",
            SETTLEMENT_ID,
            "pending",
        ])
        .output()
        .expect("admin settlement invalid transition output");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("valid utf-8");
    assert!(stderr.contains("invalid transition: completed -> pending"));
    assert!(!stderr.contains("server returned"));
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

fn quotas_set_rejects_zero_limit_before_sending() {
    let output = synapse_command()
        .args([
            "--base-url",
            "http://127.0.0.1:9",
            "admin",
            "quotas",
            "set",
            TENANT_ID,
            "0",
        ])
        .output()
        .expect("quotas set output");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("valid utf-8");
    assert!(stderr.contains("quota limit must be positive"));
}

#[test]
fn settlement_update_status_help_mentions_required_and_optional_flags() {
    let mut cmd = synapse_command();
    cmd.args(["admin", "settlements", "update-status", "--help"]);

    let output = cmd.output().expect("help output");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");

    assert!(stdout.contains("Required arguments:"));
    assert!(stdout.contains("<SETTLEMENT_ID>"));
    assert!(stdout.contains("Required flags:"));
    assert!(stdout.contains("--status <STATUS>"));
    assert!(stdout.contains("Optional flags:"));
    assert!(stdout.contains("--new-total <TOTAL>"));
    assert!(stdout.contains("--actor <ACTOR>"));
}

#[test]
fn settlement_update_status_table_and_json_modes() {
    let server = MockServer::spawn("happy");
    let base_url = server.base_url();

    let mut cmd = synapse_command();
    cmd.args([
        "--base-url",
        &base_url,
        "admin",
        "settlements",
        "update-status",
        SAMPLE_SETTLEMENT_ID,
        "--status",
        "adjusted",
        "--reason",
        "Audit correction",
        "--new-total",
        "125.0000000",
    ]);

    let output = cmd.output().expect("settlement output");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(stdout.contains("Settlement updated successfully"));
    assert!(stdout.contains("Settlement ID: 8f9b0f0c-9a89-4d1f-9d7d-0c7d7d0d9a11"));
    assert!(stdout.contains("Status: adjusted"));
    assert!(stdout.contains("Dispute reason: Audit correction"));
    assert!(stdout.contains("Original total amount: 130.0000000"));

    let mut cmd = synapse_command();
    cmd.args([
        "--base-url",
        &base_url,
        "admin",
        "settlements",
        "update-status",
        SAMPLE_SETTLEMENT_ID,
        "--status",
        "adjusted",
        "--reason",
        "Audit correction",
        "--new-total",
        "125.0000000",
        "--json",
    ]);

    let output = cmd.output().expect("settlement json output");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("valid utf-8");
    assert!(stdout.contains("\"status\": \"adjusted\""));
    assert!(stdout.contains("\"original_total_amount\": \"130.0000000\""));
    assert!(stdout.contains("\"reviewed_by\": \"admin\""));
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
        let binary =
            std::env::var_os("CARGO_BIN_EXE_mock-server").expect("mock-server binary path");
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
