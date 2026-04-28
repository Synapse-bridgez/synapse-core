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
}

pub fn run(args: ReleaseArgs) -> anyhow::Result<()> {
    let version = &args.version;
    println!("==> synapse-core release v{version}");

    ensure_clean_tree()?;

    if !args.skip_tests {
        println!("\n-- Running tests before release --");
        run_cmd("cargo", &["test", "--all"])?;
    }

    println!("\n-- Building release binary --");
    run_cmd("cargo", &["build", "--release"])?;

    if !args.skip_tag {
        create_and_push_tag(version, &args.remote)?;
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
