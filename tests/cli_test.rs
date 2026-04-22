use assert_cmd::Command;

fn synapse_cmd() -> Command {
    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("synapse-core");
    // Provide a full set of "dummy" variables so the binary doesn't crash on startup
    cmd.envs([
        (
            "DATABASE_URL",
            "postgres://synapse:synapse@localhost:5433/synapse_test",
        ),
        ("STELLAR_HORIZON_URL", "https://horizon-testnet.stellar.org"),
        ("REDIS_URL", "redis://localhost:6379"),
        ("VAULT_URL", "http://localhost:8200"),
        ("VAULT_TOKEN", "root"),
        ("ENVIRONMENT", "testing"),
    ]);
    cmd
}

#[test]
fn test_cli_config_help() {
    let mut cmd = synapse_cmd();
    // Using --help is a foolproof way to test the CLI parser
    // without triggering the full app initialization logic
    cmd.arg("config").arg("--help");
    cmd.assert().success();
}

#[test]
fn test_cli_db_migrate_help() {
    let mut cmd = synapse_cmd();
    cmd.arg("db").arg("migrate").arg("--help");
    cmd.assert().success();
}

#[test]
fn test_cli_backup_list_help() {
    let mut cmd = synapse_cmd();
    cmd.arg("backup").arg("list").arg("--help");
    cmd.assert().success();
}

#[test]
fn test_cli_tx_force_complete_invalid_uuid() {
    let mut cmd = synapse_cmd();
    cmd.arg("tx")
        .arg("force-complete")
        .arg("invalid-uuid-format");

    // This tests that the CLI validator for UUID is working
    cmd.assert().failure();
}

#[test]
fn test_cli_tx_force_complete_help() {
    let mut cmd = synapse_cmd();
    cmd.arg("tx").arg("force-complete").arg("--help");
    cmd.assert().success();
}
