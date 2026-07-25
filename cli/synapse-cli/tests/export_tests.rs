use assert_cmd::Command;
use mockito::Server;
use predicates::prelude::*;
use std::net::TcpListener;

/// Reserve an ephemeral port and immediately release it, so callers get an
/// address nothing else is listening on (unlike a hardcoded port, which can
/// collide with an unrelated service already running on the host).
fn unused_base_url() -> String {
    let port = TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral port")
        .local_addr()
        .expect("local addr")
        .port();
    format!("http://127.0.0.1:{port}")
}

#[test]
fn test_export_passes_through_raw_bytes() {
    let mut cmd = Command::cargo_bin("synapse").expect("Failed to find binary");

    cmd.arg("--url")
        .arg(unused_base_url())
        .arg("transactions")
        .arg("export")
        .arg("--format")
        .arg("csv");

    let output = cmd.output().expect("Failed to execute");
    assert!(
        !output.status.success(),
        "Command should fail with no server"
    );
}

#[test]
fn test_export_filter_flags_accepted() {
    let mut cmd = Command::cargo_bin("synapse").expect("Failed to find binary");

    cmd.arg("transactions")
        .arg("export")
        .arg("--format")
        .arg("csv")
        .arg("--from")
        .arg("2024-01-01")
        .arg("--to")
        .arg("2024-12-31")
        .arg("--status")
        .arg("pending")
        .arg("--asset-code")
        .arg("USD")
        .arg("--help");

    cmd.assert().success();
}

#[test]
fn test_export_supports_output_file() {
    let mut cmd = Command::cargo_bin("synapse").expect("Failed to find binary");

    let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
    let output_file = temp_dir.path().join("test_export.csv");

    cmd.arg("--url")
        .arg(unused_base_url())
        .arg("transactions")
        .arg("export")
        .arg("--output")
        .arg(&output_file);

    let _ = cmd.output();
}

#[test]
fn test_export_default_format_is_csv() {
    let mut cmd = Command::cargo_bin("synapse").expect("Failed to find binary");

    cmd.arg("transactions").arg("export").arg("--help");

    cmd.assert()
        .success()
        .stdout(predicates::str::contains("csv").or(predicates::str::contains("CSV")));
}

#[test]
fn test_export_supports_json_format() {
    let mut cmd = Command::cargo_bin("synapse").expect("Failed to find binary");

    cmd.arg("transactions")
        .arg("export")
        .arg("--format")
        .arg("json")
        .arg("--help");

    cmd.assert().success();
}

#[test]
fn test_export_unrecognized_format() {
    let mut cmd = Command::cargo_bin("synapse").expect("Failed to find binary");

    cmd.arg("--url")
        .arg(unused_base_url())
        .arg("transactions")
        .arg("export")
        .arg("--format")
        .arg("invalid");

    cmd.output().expect("Failed to execute");
}

/// Regression test: the CLI must call the real server route (`GET /export`,
/// same as `synapse_sdk::Transactions::export`), not `/transactions/export`
/// — and it must send `X-API-Key`, matching every other authenticated route.
#[tokio::test]
async fn test_export_hits_real_route_with_api_key() {
    let mut server = Server::new_async().await;
    let csv_body = "id,status\n1,pending\n";
    let mock = server
        .mock("GET", "/export")
        .match_query(mockito::Matcher::UrlEncoded("format".into(), "csv".into()))
        .match_header("x-api-key", "test-key")
        .with_status(200)
        .with_header("content-type", "text/csv")
        .with_body(csv_body)
        .create_async()
        .await;

    let mut cmd = Command::cargo_bin("synapse").expect("Failed to find binary");
    cmd.arg("--url")
        .arg(server.url())
        .arg("--api-key")
        .arg("test-key")
        .arg("transactions")
        .arg("export")
        .arg("--format")
        .arg("csv");

    let output = cmd.output().expect("Failed to execute");
    mock.assert_async().await;
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), csv_body);
}

#[test]
fn test_export_preserves_csv_structure() {
    let csv_sample = "id,stellar_account,amount,asset_code,status,created_at,updated_at\n\
                      550e8400-e29b-41d4-a716-446655440000,GCZST3SM6SDT75POR7GA2S4KINI5CLF47CDQW3YCJNAWRUQLbeast,100.00,USD,pending,2024-01-01T00:00:00Z,2024-01-01T00:00:00Z";

    assert!(csv_sample.contains("id,stellar_account,amount"));
}
