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
