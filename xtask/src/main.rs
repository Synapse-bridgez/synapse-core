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
}

fn main() -> Result<()> {
	let cli = Cli::parse();
	match cli.command {
		Command::CapacityForecast(args) => commands::capacity_forecast::run(args),
		Command::Scorecard(args) => commands::scorecard::run(args),
	}
}
