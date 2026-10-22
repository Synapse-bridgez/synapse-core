use super::run_cmd;
use clap::Args;
use std::thread;
use std::time::Duration;

/// Arguments for `cargo xtask setup`
#[derive(Args)]
pub struct SetupArgs {
    /// Skip starting docker-compose services (useful if they are already running).
    #[arg(long)]
    pub skip_docker: bool,

    /// Skip running database migrations.
    #[arg(long)]
    pub skip_migrations: bool,

    /// Skip seeding initial data.
    #[arg(long)]
    pub skip_seed: bool,

    /// Tear down the dev profile and reset seeded data before re-seeding.
    #[arg(long)]
    pub reset: bool,

    /// Skip the interactive confirmation for `--reset` (still refuses non-local hosts).
    #[arg(long)]
    pub yes: bool,

    /// Path to the docker-compose file to use.
    #[arg(long, default_value = "docker-compose.dev.yml")]
    pub compose_file: String,
}

pub fn run(args: SetupArgs) -> anyhow::Result<()> {
    println!("==> synapse-core setup (local development profile)");

    if args.reset {
        reset_profile(&args)?;
    }

    if !args.skip_docker {
        start_docker(&args.compose_file)?;
        wait_for_postgres()?;
    }

    if !args.skip_migrations {
        run_migrations()?;
    }

    if !args.skip_seed {
        seed_data()?;
    }

    print_endpoints();
    Ok(())
}

/// Tear down the dev profile and clear seeded data.
///
/// Guarded so it can only ever run against a local development database.
fn reset_profile(args: &SetupArgs) -> anyhow::Result<()> {
    println!("\n-- Resetting local development profile --");
    guard_local_database()?;

    if !args.yes {
        confirm_reset()?;
    }

    if !args.skip_docker {
        println!("  Stopping docker-compose services and removing volumes...");
        run_cmd(
            "docker",
            &[
                "compose",
                "-f",
                &args.compose_file,
                "down",
                "-v",
                "--remove-orphans",
            ],
        )?;
    }

    println!("  Reset complete — the profile will be recreated from scratch.");
    Ok(())
}

/// Refuse to run destructive operations against anything that is not clearly local.
fn guard_local_database() -> anyhow::Result<()> {
    let database_url = std::env::var("DATABASE_URL").unwrap_or_default();
    if database_url.is_empty() {
        anyhow::bail!(
            "DATABASE_URL is not set. Source `.env.development` (e.g. `set -a; . ./.env.development; set +a`) \
             or export a local DATABASE_URL before running `--reset`."
        );
    }

    let host = database_host(&database_url);
    let is_local = matches!(
        host.as_deref(),
        Some("localhost") | Some("127.0.0.1") | Some("::1") | Some("postgres") | Some("db")
    );

    if !is_local {
        anyhow::bail!(
            "Refusing to reset: DATABASE_URL points at host `{}`, which is not a local development host. \
             `--reset` is only permitted for localhost/127.0.0.1/::1/postgres/db.",
            host.unwrap_or_else(|| "<unknown>".to_string())
        );
    }

    Ok(())
}

/// Extract the host portion of a `postgres://user:pass@host:port/db` style URL.
fn database_host(database_url: &str) -> Option<String> {
    let after_scheme = database_url.split("://").nth(1)?;
    let authority = after_scheme.split('/').next()?;
    let host_port = match authority.rsplit_once('@') {
        Some((_, host_port)) => host_port,
        None => authority,
    };
    if host_port.is_empty() {
        return None;
    }
    // Handle bracketed IPv6 literals such as [::1]:5432.
    if let Some(rest) = host_port.strip_prefix('[') {
        let end = rest.find(']')?;
        return Some(rest[..end].to_string());
    }
    let host = host_port.split(':').next()?;
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

fn confirm_reset() -> anyhow::Result<()> {
    use std::io::{self, Write};

    print!("  This will delete all local dev data. Continue? [y/N] ");
    io::stdout().flush().ok();

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    if matches!(input.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        anyhow::bail!("Reset aborted by user.")
    }
}

fn start_docker(compose_file: &str) -> anyhow::Result<()> {
    println!("\n-- Starting docker-compose services ({compose_file}) --");
    run_cmd(
        "docker",
        &["compose", "-f", compose_file, "up", "-d", "--wait"],
    )
    .map_err(|e| {
        anyhow::anyhow!(
            "Failed to start the dev profile via `docker compose -f {compose_file} up -d --wait`: {e}\n\
             Hint: ensure Docker is running and the compose file exists. \
             You can skip this step with `--skip-docker` if services are already up."
        )
    })?;
    Ok(())
}

fn wait_for_postgres() -> anyhow::Result<()> {
    println!("\n-- Waiting for PostgreSQL to be ready --");
    let max_attempts = 20u32;
    for attempt in 1..=max_attempts {
        let status = std::process::Command::new("docker")
            .args(["exec", "synapse-postgres", "pg_isready", "-U", "synapse"])
            .status();

        match status {
            Ok(s) if s.success() => {
                println!("  PostgreSQL is ready.");
                return Ok(());
            }
            _ => {
                println!("  Attempt {attempt}/{max_attempts} — not ready yet, retrying...");
                thread::sleep(Duration::from_secs(3));
            }
        }
    }
    anyhow::bail!("PostgreSQL did not become ready in time.")
}

fn run_migrations() -> anyhow::Result<()> {
    println!("\n-- Running database migrations --");
    // sqlx migrate run reads DATABASE_URL from the environment.
    run_cmd("sqlx", &["migrate", "run"]).map_err(|e| {
        anyhow::anyhow!(
            "Failed to apply migrations: {e}\n\
             Hint: ensure DATABASE_URL is set (source `.env.development`) and PostgreSQL is reachable. \
             Skip with `--skip-migrations` if the schema is already up to date."
        )
    })?;
    Ok(())
}

fn seed_data() -> anyhow::Result<()> {
    println!("\n-- Seeding deterministic demo data --");
    // The seed binary is idempotent: it upserts fixed ids/timestamps so repeated
    // runs converge on the same state. It goes through the service's persistence
    // layer rather than ad-hoc SQL.
    run_cmd("cargo", &["run", "--bin", "seed", "--", "--profile", "development"]).map_err(|e| {
        anyhow::anyhow!(
            "Failed to seed demo data: {e}\n\
             Hint: ensure migrations have run and DATABASE_URL points at the local dev database. \
             Skip with `--skip-seed` if you only need the schema."
        )
    })?;
    println!("  Seed complete.");
    Ok(())
}

fn print_endpoints() {
    let base = std::env::var("SERVER_URL").unwrap_or_else(|_| "http://localhost:8080".to_string());
    println!("\n==> Ready-to-use endpoints");
    println!("  API base:        {base}");
    println!("  Health check:    {base}/health");
    println!("  Swap orders:     {base}/api/v1/swap/orders");
    println!("  Liquidity:       {base}/api/v1/swap/liquidity");
    println!("\n✓ Setup complete — run `cargo run` to start the server.");
    println!("  Re-run any time (idempotent): `cargo xtask setup`");
    println!("  Reset local data:             `cargo xtask setup --reset`");
}
