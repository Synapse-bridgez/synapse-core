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

    /// Run the seed idempotency smoke test (requires a running dev database).
    #[arg(long)]
    pub seed_smoke: bool,

    /// Pass extra arguments directly to `cargo test` (e.g. -- --nocapture).
    #[arg(last = true)]
    pub extra: Vec<String>,
}

pub fn run(args: TestArgs) -> anyhow::Result<()> {
    println!("==> synapse-core test");

    if args.seed_smoke {
        run_seed_smoke_test()?;
        println!("\n✓ Seed smoke test passed.");
        return Ok(());
    }

    if !args.integration_only {
        run_unit_tests(&args.extra)?;
    }

    if !args.unit_only {
        run_integration_tests(&args.extra)?;
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

/// Expected deterministic row counts produced by the seed tooling.
///
/// These values must stay in sync with the seed module under `xtask/src/seed`.
/// They are asserted after each seed run to prove idempotency.
const EXPECTED_TENANTS: i64 = 2;
const EXPECTED_ASSETS: i64 = 4;
const EXPECTED_LIQUIDITY_PROVIDERS: i64 = 3;
const EXPECTED_SWAP_ORDERS: i64 = 10;
const EXPECTED_SWAP_FILLS: i64 = 6;

/// Smoke test that runs the seeding command twice and asserts:
/// 1. The first run succeeds and produces the expected row counts.
/// 2. The second run succeeds (idempotent) and produces identical row counts.
///
/// Requires a reachable development database. The seed command itself guards
/// against non-development hosts, so this test is safe to run locally.
fn run_seed_smoke_test() -> anyhow::Result<()> {
    println!("\n-- Seed idempotency smoke test --");

    println!("  [1/3] Resetting and seeding demo data (first run)...");
    run_cmd("cargo", &["run", "--bin", "seed", "--", "--reset"])?;
    let first = read_row_counts()?;
    assert_counts("first run", &first)?;

    println!("  [2/3] Re-running seed (second run, must be idempotent)...");
    run_cmd("cargo", &["run", "--bin", "seed"])?;
    let second = read_row_counts()?;
    assert_counts("second run", &second)?;

    println!("  [3/3] Comparing row counts between runs...");
    if first != second {
        anyhow::bail!(
            "seed is not idempotent: row counts differ between runs\n  first:  {:?}\n  second: {:?}",
            first,
            second
        );
    }

    println!(
        "  ✓ Idempotent: tenants={}, assets={}, liquidity_providers={}, swap_orders={}, swap_fills={}",
        second.tenants,
        second.assets,
        second.liquidity_providers,
        second.swap_orders,
        second.swap_fills
    );
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RowCounts {
    tenants: i64,
    assets: i64,
    liquidity_providers: i64,
    swap_orders: i64,
    swap_fills: i64,
}

/// Reads row counts from the development database using the seed binary's
/// `--counts` mode, which prints a stable `key=value` summary on stdout.
fn read_row_counts() -> anyhow::Result<RowCounts> {
    let output = std::process::Command::new("cargo")
        .args(["run", "--quiet", "--bin", "seed", "--", "--counts"])
        .output()?;

    if !output.status.success() {
        anyhow::bail!(
            "seed --counts failed with status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut counts = RowCounts {
        tenants: -1,
        assets: -1,
        liquidity_providers: -1,
        swap_orders: -1,
        swap_fills: -1,
    };

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (key, value) = match line.split_once('=') {
            Some(kv) => kv,
            None => continue,
        };
        let value: i64 = value
            .trim()
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid count for `{key}`: {e}"))?;
        match key.trim() {
            "tenants" => counts.tenants = value,
            "assets" => counts.assets = value,
            "liquidity_providers" => counts.liquidity_providers = value,
            "swap_orders" => counts.swap_orders = value,
            "swap_fills" => counts.swap_fills = value,
            _ => {}
        }
    }

    if counts.tenants < 0
        || counts.assets < 0
        || counts.liquidity_providers < 0
        || counts.swap_orders < 0
        || counts.swap_fills < 0
    {
        anyhow::bail!(
            "seed --counts did not report all expected keys; got:\n{}",
            stdout
        );
    }

    Ok(counts)
}

fn assert_counts(label: &str, counts: &RowCounts) -> anyhow::Result<()> {
    let checks = [
        ("tenants", counts.tenants, EXPECTED_TENANTS),
        ("assets", counts.assets, EXPECTED_ASSETS),
        (
            "liquidity_providers",
            counts.liquidity_providers,
            EXPECTED_LIQUIDITY_PROVIDERS,
        ),
        ("swap_orders", counts.swap_orders, EXPECTED_SWAP_ORDERS),
        ("swap_fills", counts.swap_fills, EXPECTED_SWAP_FILLS),
    ];

    let mut mismatches = Vec::new();
    for (name, actual, expected) in checks {
        if actual != expected {
            mismatches.push(format!("{name}: expected {expected}, got {actual}"));
        }
    }

    if !mismatches.is_empty() {
        anyhow::bail!(
            "seed row counts mismatch on {label}:\n  {}",
            mismatches.join("\n  ")
        );
    }

    Ok(())
}