use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn test_bash_completions() {
    let mut cmd = Command::cargo_bin("synapse").unwrap();
    let assert = cmd.arg("completions").arg("bash").assert();

    assert.success();
    assert.stdout(predicate::str::contains("_synapse()"));
}

#[test]
fn test_zsh_completions() {
    let mut cmd = Command::cargo_bin("synapse").unwrap();
    let assert = cmd.arg("completions").arg("zsh").assert();

    assert.success();
    assert.stdout(predicate::str::contains("compdef _synapse synapse"));
}

#[test]
fn test_fish_completions() {
    let mut cmd = Command::cargo_bin("synapse").unwrap();
    let assert = cmd.arg("completions").arg("fish").assert();

    assert.success();
    assert.stdout(predicate::str::contains("complete -c synapse"));
}

#[test]
fn test_invalid_shell() {
    let mut cmd = Command::cargo_bin("synapse").unwrap();
    let assert = cmd.arg("completions").arg("invalid").assert();

    assert.failure();
    assert.stderr(predicate::str::contains("Unsupported shell"));
}
