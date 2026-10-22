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
	/// Validate swap lifecycle event schemas and golden sample payloads.
	SchemaValidate(commands::schema_validate::SchemaValidateArgs),
}

fn main() -> Result<()> {
	let cli = Cli::parse();
	match cli.command {
		Command::CapacityForecast(args) => commands::capacity_forecast::run(args),
		Command::Scorecard(args) => commands::scorecard::run(args),
		Command::Postmortem(args) => commands::postmortem::run(args),
		Command::SchemaValidate(args) => commands::schema_validate::run(args),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use clap::CommandFactory;

	#[test]
	fn cli_verifies() {
		Cli::command().debug_assert();
	}

	#[test]
	fn schema_validate_subcommand_is_registered() {
		let cmd = Cli::command();
		let sub = cmd
			.get_subcommands()
			.find(|c| c.get_name() == "schema-validate");
		assert!(
			sub.is_some(),
			"expected `schema-validate` subcommand to be registered"
		);
	}
}