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
	/// Collect SOC 2 control evidence or render the control matrix.
	Compliance(commands::compliance::ComplianceArgs),
	/// Generate release notes for Wave-linked merged PRs, grouped by category and complexity.
	ReleaseNotes(commands::release_notes::ReleaseNotesArgs),
}

fn main() -> Result<()> {
	let cli = Cli::parse();
	match cli.command {
		Command::CapacityForecast(args) => commands::capacity_forecast::run(args),
		Command::Scorecard(args) => commands::scorecard::run(args),
		Command::Postmortem(args) => commands::postmortem::run(args),
		Command::Compliance(args) => commands::compliance::run(args),
		Command::ReleaseNotes(args) => commands::release_notes::run(args),
	}
}
