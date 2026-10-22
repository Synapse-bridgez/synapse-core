use super::run_cmd;
use clap::Args;

/// Arguments for `cargo xtask test`
#[derive(Args)]
pub struct TestArgs {
    /// Run only unit tests (skip integration tests).
    #[arg(long)]
    pub unit_only: bool,

    /// Run only integration tests (skip unit tests).
    #[arg(long)]
    pub integration_only: bool,

    /// Skip swap lifecycle event schema validation.
    #[arg(long)]
    pub skip_schema_validation: bool,

    /// Pass extra arguments directly to `cargo test` (e.g. -- --nocapture).
    #[arg(last = true)]
    pub extra: Vec<String>,
}

pub fn run(args: TestArgs) -> anyhow::Result<()> {
    println!("==> synapse-core test");

    if !args.integration_only {
        run_unit_tests(&args.extra)?;
    }

    if !args.unit_only {
        run_integration_tests(&args.extra)?;
    }

    if !args.skip_schema_validation {
        run_schema_validation(&args.extra)?;
    }

    println!("\n✓ All tests passed.");
    Ok(())
}

fn run_unit_tests(extra: &[String]) -> anyhow::Result<()> {
    println!("\n-- Unit tests --");
    let mut cmd_args = vec!["test", "--lib", "--all"];
    let extra_strs: Vec<&str> = extra.iter().map(String::as_str).collect();
    cmd_args.extend_from_slice(&extra_strs);
    run_cmd("cargo", &cmd_args)?;
    Ok(())
}

fn run_integration_tests(extra: &[String]) -> anyhow::Result<()> {
    println!("\n-- Integration tests --");
    let mut cmd_args = vec!["test", "--test", "*", "--all"];
    let extra_strs: Vec<&str> = extra.iter().map(String::as_str).collect();
    cmd_args.extend_from_slice(&extra_strs);
    run_cmd("cargo", &cmd_args)?;
    Ok(())
}

/// Runs the swap lifecycle event contract tests:
///
/// * `contract_tests` validates representative payloads for every swap
///   lifecycle event against its JSON Schema in `schemas/`.
/// * `golden_fixture_tests` compares emitted payloads against checked-in
///   golden fixtures so accidental field renames or type changes fail CI.
/// * `compatibility_tests` validates a previous-version consumer against the
///   current producer to enforce the documented compatibility policy.
fn run_schema_validation(extra: &[String]) -> anyhow::Result<()> {
    println!("\n-- Swap lifecycle event schema validation --");

    let extra_strs: Vec<&str> = extra.iter().map(String::as_str).collect();

    for target in ["contract_tests", "golden_fixture_tests", "compatibility_tests"] {
        println!("\n   -> {target}");
        let mut cmd_args = vec!["test", "--test", target, "--all"];
        cmd_args.extend_from_slice(&extra_strs);
        run_cmd("cargo", &cmd_args)?;
    }

    Ok(())
}