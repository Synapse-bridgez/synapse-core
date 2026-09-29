use super::compat;
use super::run_cmd;
use clap::Args;
use std::process::Command;

/// Arguments for `cargo xtask release`
#[derive(Args)]
pub struct ReleaseArgs {
    /// Version to release (e.g. 1.2.3). Falls back to the VERSION environment variable.
    #[arg(long, env = "VERSION")]
    pub version: String,

    /// Skip running tests before building the release.
    #[arg(long)]
    pub skip_tests: bool,

    /// Skip creating and pushing the git tag.
    #[arg(long)]
    pub skip_tag: bool,

    /// Remote to push the tag to.
    #[arg(long, default_value = "origin")]
    pub remote: String,

    /// Skip pushing release artifacts after the local build completes.
    #[arg(long)]
    pub skip_push: bool,

    /// Skip generating the post-release reliability scorecard report.
    #[arg(long)]
    pub skip_scorecard: bool,

    /// Length of the before/after comparison window, in hours.
    #[arg(long, default_value_t = 24)]
    pub scorecard_window_hours: u64,
}

pub fn run(args: ReleaseArgs) -> anyhow::Result<()> {
    let version = &args.version;
    println!("==> synapse-core release v{version}");

    ensure_clean_tree()?;
    compat::ensure_matrix_has_entry_for(version)?;

    if !args.skip_tests {
        println!("\n-- Running tests before release --");
        run_cmd("cargo", &["test", "--all"])?;
    }

    println!("\n-- Building release binary --");
    run_cmd("cargo", &["build", "--release"])?;

    if !args.skip_tag && !args.skip_push {
        create_and_push_tag(version, &args.remote)?;
    }

    if !args.skip_scorecard {
        generate_scorecard(version, args.scorecard_window_hours)?;
    }

    println!("\n✓ Release v{version} complete.");
    Ok(())
}

fn ensure_clean_tree() -> anyhow::Result<()> {
    println!("\n-- Checking for a clean git working tree --");
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to run git status: {e}"))?;

    if !output.stdout.is_empty() {
        anyhow::bail!(
            "Working tree is not clean. Commit or stash your changes before releasing.\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
    println!("  Working tree is clean.");
    Ok(())
}

fn create_and_push_tag(version: &str, remote: &str) -> anyhow::Result<()> {
    let tag = format!("v{version}");
    println!("\n-- Creating git tag {tag} --");
    run_cmd("git", &["tag", "-a", &tag, "-m", &format!("Release {tag}")])?;

    println!("-- Pushing tag {tag} to {remote} --");
    run_cmd("git", &["push", remote, &tag])?;
    Ok(())
}

/// Generate the per-release reliability scorecard as a post-release step.
///
/// Compares error rate, p50/p95/p99 latency, and incident/alert counts over an
/// equivalent window before and after the release. The comparison is delegated
/// to the `synapse-core` binary so the same trend-analysis logic used by the
/// capacity forecasting tool is reused, and so overlapping windows (release B's
/// "before" window overlapping release A's "after" window) are handled by the
/// shared implementation rather than re-derived here.
fn generate_scorecard(version: &str, window_hours: u64) -> anyhow::Result<()> {
    println!("\n-- Generating reliability scorecard for v{version} --");
    let window = format!("{window_hours}h");
    run_cmd(
        "cargo",
        &[
            "run",
            "--release",
            "--bin",
            "synapse-core",
            "--",
            "scorecard",
            "--release",
            version,
            "--window",
            &window,
        ],
    )?;
    println!("  Scorecard written for v{version} (window {window}).");
    Ok(())
}
