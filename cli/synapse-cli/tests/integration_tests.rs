use assert_cmd::Command;
use predicates::prelude::*;
use std::net::TcpListener;

fn synapse_cmd() -> Command {
    Command::cargo_bin("synapse").expect("Failed to find binary")
}

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
fn test_export_csv_default_format() {
    let mut cmd = synapse_cmd();
    cmd.arg("--url")
        .arg(unused_base_url())
        .arg("transactions")
        .arg("export");

    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("connection").or(predicate::str::contains("error")));
}

#[test]
fn test_export_with_filters() {
    let mut cmd = synapse_cmd();
    cmd.arg("--url")
        .arg(unused_base_url())
        .arg("transactions")
        .arg("export")
        .arg("--format")
        .arg("csv")
        .arg("--status")
        .arg("pending")
        .arg("--asset-code")
        .arg("USD");

    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("connection").or(predicate::str::contains("error")));
}

#[test]
fn test_export_json_format() {
    let mut cmd = synapse_cmd();
    cmd.arg("--url")
        .arg(unused_base_url())
        .arg("transactions")
        .arg("export")
        .arg("--format")
        .arg("json");

    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("connection").or(predicate::str::contains("error")));
}

#[test]
fn test_export_with_date_filters() {
    let mut cmd = synapse_cmd();
    cmd.arg("--url")
        .arg(unused_base_url())
        .arg("transactions")
        .arg("export")
        .arg("--from")
        .arg("2024-01-01")
        .arg("--to")
        .arg("2024-12-31");

    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("connection").or(predicate::str::contains("error")));
}

#[test]
fn test_export_to_file() {
    let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
    let output_file = temp_dir.path().join("export.csv");

    let mut cmd = synapse_cmd();
    cmd.arg("--url")
        .arg(unused_base_url())
        .arg("transactions")
        .arg("export")
        .arg("--output")
        .arg(&output_file);

    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("connection").or(predicate::str::contains("error")));
}

#[test]
fn test_settlements_list_help() {
    let mut cmd = synapse_cmd();
    cmd.arg("settlements").arg("list").arg("--help");

    cmd.assert().success();
}

#[test]
fn test_settlements_get_help() {
    let mut cmd = synapse_cmd();
    cmd.arg("settlements").arg("get").arg("--help");

    cmd.assert().success();
}

#[test]
fn test_export_help() {
    let mut cmd = synapse_cmd();
    cmd.arg("transactions").arg("export").arg("--help");

    cmd.assert().success();
}

#[test]
fn test_export_with_all_filters() {
    let mut cmd = synapse_cmd();
    cmd.arg("--url")
        .arg(unused_base_url())
        .arg("transactions")
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
        .arg("USD");

    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("connection").or(predicate::str::contains("error")));
}
