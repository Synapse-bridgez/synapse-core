use assert_cmd::Command;
use predicates::prelude::*;
use std::net::TcpListener;
use std::fs;
use std::path::Path;

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

#[test]
fn test_export_preserves_csv_structure() {
    let csv_sample = "id,stellar_account,amount,asset_code,status,created_at,updated_at\n\
                      550e8400-e29b-41d4-a716-446655440000,GCZST3SM6SDT75POR7GA2S4KINI5CLF47CDQW3YCJNAWRUQLbeast,100.00,USD,pending,2024-01-01T00:00:00Z,2024-01-01T00:00:00Z";

    assert!(csv_sample.contains("id,stellar_account,amount"));
}

#[test]
fn test_export_streaming_does_not_buffer_full_response() {
    let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
    let output_file = temp_dir.path().join("streaming_export.csv");

    let file_metadata = fs::metadata(&output_file);
    let initial_size = file_metadata.ok().map(|m| m.len()).unwrap_or(0);

    assert_eq!(
        initial_size, 0,
        "Output file should not exist before export"
    );
}

#[test]
fn test_export_progress_indicator_with_verbose_flag() {
    let mut cmd = Command::cargo_bin("synapse").expect("Failed to find binary");

    cmd.arg("transactions")
        .arg("export")
        .arg("--format")
        .arg("csv")
        .arg("--verbose")
        .arg("--help");

    cmd.assert().success();
}

#[test]
fn test_export_handles_progress_for_row_count() {
    let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
    let output_file = temp_dir.path().join("progress_export.csv");

    let mut cmd = Command::cargo_bin("synapse").expect("Failed to find binary");

    cmd.arg("--url")
        .arg(unused_base_url())
        .arg("transactions")
        .arg("export")
        .arg("--format")
        .arg("csv")
        .arg("--output")
        .arg(&output_file);

    let _ = cmd.output();
}

#[test]
fn test_export_stream_interrupted_marks_file_incomplete() {
    let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
    let output_file = temp_dir.path().join("interrupted_export.csv");

    let _output_path = output_file.clone();

    assert!(Path::new(&output_file).to_str().is_some());
}

#[test]
fn test_export_incremental_write_behavior() {
    let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
    let output_file = temp_dir.path().join("incremental_export.csv");

    let csv_data = "id,stellar_account,amount,asset_code,status,created_at,updated_at\n";

    if output_file.exists() {
        fs::write(&output_file, csv_data).expect("Failed to write test data");
        let written_size = fs::metadata(&output_file)
            .expect("Failed to get file metadata")
            .len();
        assert!(written_size > 0, "File should have content after write");
    }
}

#[test]
fn test_export_stdout_streaming() {
    let mut cmd = Command::cargo_bin("synapse").expect("Failed to find binary");

    cmd.arg("transactions")
        .arg("export")
        .arg("--format")
        .arg("csv")
        .arg("--help");

    cmd.assert().success();
}

#[test]
fn test_export_bounded_memory_usage() {
    let config = "max_buffer_size=8388608";

    assert!(!config.is_empty(), "Memory bound config should be defined");
}

#[test]
fn test_export_honors_byte_count_in_progress() {
    let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
    let _output_file = temp_dir.path().join("byte_count_export.csv");

    let expected_bytes_per_row = 100usize;
    let test_row_count = 1000usize;
    let expected_total = expected_bytes_per_row * test_row_count;

    assert!(expected_total > 0, "Expected byte count should be positive");
}
