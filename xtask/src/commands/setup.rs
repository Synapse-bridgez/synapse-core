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

    /// Path to the docker-compose file to use.
    #[arg(long, default_value = "docker-compose.dev.yml")]
    pub compose_file: String,
}

pub fn run(args: SetupArgs) -> anyhow::Result<()> {
    println!("==> synapse-core setup");

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

    println!("\n✓ Setup complete — run `cargo run` to start the server.");
    Ok(())
}

fn start_docker(compose_file: &str) -> anyhow::Result<()> {
    println!("\n-- Starting docker-compose services ({compose_file}) --");
    run_cmd(
        "docker",
        &["compose", "-f", compose_file, "up", "-d", "--wait"],
    )?;
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
    run_cmd("sqlx", &["migrate", "run"])?;
    Ok(())
}

fn seed_data() -> anyhow::Result<()> {
    println!("\n-- Seeding initial data --");
    // Feature flags seed — non-fatal if psql is unavailable or row already exists.
    let result = run_cmd(
        "psql",
        &[
            "${DATABASE_URL}",
            "-c",
            "INSERT INTO feature_flags (name, enabled) VALUES ('dev_seed', true) ON CONFLICT DO NOTHING;",
        ],
    );
    if let Err(e) = result {
        println!("  Warning: seed step skipped or failed (non-fatal): {e}");
    }
    println!("  Seed complete.");
    Ok(())
}
