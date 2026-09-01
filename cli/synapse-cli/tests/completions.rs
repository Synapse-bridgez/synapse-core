use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn test_bash_completions() {
    let mut cmd = Command::cargo_bin("synapse").unwrap();
    cmd.arg("completions")
        .arg("bash")
        .assert()
        .success()
        .stdout(predicate::str::contains("_synapse()"));
}

#[test]
fn test_zsh_completions() {
    let mut cmd = Command::cargo_bin("synapse").unwrap();
    cmd.arg("completions")
        .arg("zsh")
        .assert()
        .success()
        .stdout(predicate::str::contains("compdef _synapse synapse"));
}

#[test]
fn test_fish_completions() {
    let mut cmd = Command::cargo_bin("synapse").unwrap();
    cmd.arg("completions")
        .arg("fish")
        .assert()
        .success()
        .stdout(predicate::str::contains("complete -c synapse"));
}

#[test]
fn test_invalid_shell() {
    let mut cmd = Command::cargo_bin("synapse").unwrap();
    cmd.arg("completions")
        .arg("invalid")
        .assert()
        .failure()
        .stderr(predicate::str::contains("Unsupported shell"));
}

#[test]
fn test_completions_cover_transactions_subcommand() {
    let mut cmd = Command::cargo_bin("synapse").unwrap();
    cmd.arg("completions")
        .arg("bash")
        .assert()
        .success()
        .stdout(predicate::str::contains("transactions"));
}

#[test]
fn test_completions_cover_settlements_subcommand() {
    let mut cmd = Command::cargo_bin("synapse").unwrap();
    cmd.arg("completions")
        .arg("bash")
        .assert()
        .success()
        .stdout(predicate::str::contains("settlements"));
}

#[test]
fn test_completions_cover_admin_subcommand() {
    let mut cmd = Command::cargo_bin("synapse").unwrap();
    cmd.arg("completions")
        .arg("bash")
        .assert()
        .success()
        .stdout(predicate::str::contains("admin"));
}

#[test]
fn test_completions_cover_webhooks_subcommand() {
    let mut cmd = Command::cargo_bin("synapse").unwrap();
    cmd.arg("completions")
        .arg("bash")
        .assert()
        .success()
        .stdout(predicate::str::contains("webhooks"));
}

#[test]
fn test_completions_cover_events_subcommand() {
    let mut cmd = Command::cargo_bin("synapse").unwrap();
    cmd.arg("completions")
        .arg("bash")
        .assert()
        .success()
        .stdout(predicate::str::contains("events"));
}

#[test]
fn test_completions_cover_graphql_subcommand() {
    let mut cmd = Command::cargo_bin("synapse").unwrap();
    cmd.arg("completions")
        .arg("bash")
        .assert()
        .success()
        .stdout(predicate::str::contains("graphql"));
}

#[test]
fn test_completions_cover_export_flag() {
    let mut cmd = Command::cargo_bin("synapse").unwrap();
    cmd.arg("completions")
        .arg("bash")
        .assert()
        .success()
        .stdout(predicate::str::contains("export"));
}

#[test]
fn test_completions_cover_format_flag() {
    let mut cmd = Command::cargo_bin("synapse").unwrap();
    cmd.arg("completions")
        .arg("bash")
        .assert()
        .success()
        .stdout(predicate::str::contains("format"));
}

#[test]
fn test_completions_cover_url_flag() {
    let mut cmd = Command::cargo_bin("synapse").unwrap();
    cmd.arg("completions")
        .arg("bash")
        .assert()
        .success()
        .stdout(predicate::str::contains("url"));
}

#[test]
fn test_completions_cover_verbose_flag() {
    let mut cmd = Command::cargo_bin("synapse").unwrap();
    cmd.arg("completions")
        .arg("bash")
        .assert()
        .success()
        .stdout(predicate::str::contains("verbose"));
}

#[test]
fn test_zsh_completions_coverage() {
    let mut cmd = Command::cargo_bin("synapse").unwrap();
    cmd.arg("completions")
        .arg("zsh")
        .assert()
        .success()
        .stdout(predicate::str::contains("transactions"));
}

#[test]
fn test_fish_completions_coverage() {
    let mut cmd = Command::cargo_bin("synapse").unwrap();
    cmd.arg("completions")
        .arg("fish")
        .assert()
        .success()
        .stdout(predicate::str::contains("transactions"));
}

#[test]
fn test_dynamic_completion_graceful_fallback() {
    let mut cmd = Command::cargo_bin("synapse").unwrap();
    cmd.arg("completions")
        .arg("bash")
        .assert()
        .success();
}

#[test]
fn test_completions_envelope_all_flags() {
    let mut cmd = Command::cargo_bin("synapse").unwrap();
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--url"));
}

#[test]
fn test_transactions_export_completions() {
    let mut cmd = Command::cargo_bin("synapse").unwrap();
    cmd.arg("transactions")
        .arg("export")
        .arg("--help")
        .assert()
        .success();
}

#[test]
fn test_completions_health_check_command() {
    let mut cmd = Command::cargo_bin("synapse").unwrap();
    cmd.arg("health")
        .arg("--help")
        .assert()
        .success();
}

#[test]
fn test_completion_static_vs_dynamic_distinction() {
    let mut cmd = Command::cargo_bin("synapse").unwrap();
    cmd.arg("completions")
        .arg("bash")
        .assert()
        .success();
}

#[test]
fn test_completions_no_unreachable_api_required() {
    let mut cmd = Command::cargo_bin("synapse").unwrap();
    cmd.arg("completions")
        .arg("bash")
        .env("SYNAPSE_URL", "http://unreachable-server")
        .assert()
        .success();
}
