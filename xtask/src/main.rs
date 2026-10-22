use anyhow::Result;
use clap::{Parser, Subcommand};

mod commands;

#[derive(Parser)]
#[command(name = "xtask", about = "Development and operations task runner")]
struct Cli {
	#[command(subcommand)]
	command: Command,
}

#[derive(Subcommand)]
enum Command {
	/// Forecast infrastructure capacity from historical Prometheus metrics.
	CapacityForecast(commands::capacity_forecast::CapacityForecastArgs),
	/// Compare production reliability metrics around a release.
	Scorecard(commands::scorecard::ScorecardArgs),
	/// Generate a postmortem draft from alert, metric, and deployment history.
	Postmortem(commands::postmortem::PostmortemArgs),
	/// Seed deterministic local development data (tenants, assets, liquidity, orders, fills).
	Seed(commands::seed::SeedArgs),
	/// Tear down seeded local development data.
	Teardown(commands::seed::TeardownArgs),
	/// Lint migrations for unsafe expand/contract violations.
	Lint(commands::migration::LintArgs),
	/// Rehearse rollback of all migrations against a populated test database.
	RollbackRehearsal(commands::migration::RollbackRehearsalArgs),
}

fn main() -> Result<()> {
	let cli = Cli::parse();
	match cli.command {
		Command::CapacityForecast(args) => commands::capacity_forecast::run(args),
		Command::Scorecard(args) => commands::scorecard::run(args),
		Command::Postmortem(args) => commands::postmortem::run(args),
		Command::Seed(args) => commands::seed::run_seed(args),
		Command::Teardown(args) => commands::seed::run_teardown(args),
		Command::Lint(args) => commands::migration::run_lint(args),
		Command::RollbackRehearsal(args) => commands::migration::run_rollback_rehearsal(args),
	}
}