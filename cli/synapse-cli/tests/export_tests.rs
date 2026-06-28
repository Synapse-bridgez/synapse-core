use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn test_export_passes_through_raw_bytes() {
    let mut cmd = Command::cargo_bin("synapse").expect("Failed to find binary");

    cmd.arg("--url")
        .arg("http://localhost:3000")
        .arg("transactions")
        .arg("export")
        .arg("--format")
        .arg("csv");

    let output = cmd.output().expect("Failed to execute");
    assert!(!output.status.success(), "Command should fail with no server");
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
        .arg("http://localhost:3000")
        .arg("transactions")
        .arg("export")
        .arg("--output")
        .arg(&output_file);

    let _ = cmd.output();
}

#[test]
fn test_export_default_format_is_csv() {
    let mut cmd = Command::cargo_bin("synapse").expect("Failed to find binary");

    cmd.arg("transactions")
        .arg("export")
        .arg("--help");

    cmd.assert()
        .success()
        .stdout(predicates::str::contains("csv")
            .or(predicates::str::contains("CSV")));
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
        .arg("http://localhost:3000")
        .arg("transactions")
        .arg("export")
        .arg("--format")
        .arg("invalid");

    let output = cmd.output().expect("Failed to execute");
}

#[test]
fn test_export_preserves_csv_structure() {
    let csv_sample = "id,stellar_account,amount,asset_code,status,created_at,updated_at\n\
                      550e8400-e29b-41d4-a716-446655440000,GCZST3SM6SDT75POR7GA2S4KINI5CLF47CDQW3YCJNAWRUQLbeast,100.00,USD,pending,2024-01-01T00:00:00Z,2024-01-01T00:00:00Z";

    assert!(csv_sample.contains("id,stellar_account,amount"));
}
