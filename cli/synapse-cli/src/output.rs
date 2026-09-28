use anyhow::Result;
use serde::Serialize;

/// Render a value as either pretty-printed JSON or a table string produced by
/// the provided `table_renderer` closure.
use crate::formatter::{Formatter, OutputFormat};

pub fn render<T, F>(value: &T, json: bool, table_renderer: F) -> Result<String>
where
    T: Serialize,
    F: FnOnce(&T) -> String,
{
    if json {
        Formatter::format_json_output(value, OutputFormat::Json)
    } else {
        Ok(table_renderer(value))
    }
}

/// Format and print a serializable value to stdout.
pub fn format_output<T: Serialize>(data: &T, json: bool) {
    if json {
        match serde_json::to_string_pretty(data) {
            Ok(output) => println!("{}", output),
            Err(e) => eprintln!("Failed to serialize as JSON: {}", e),
        }
    } else {
        match serde_json::to_value(data) {
            Ok(v) => println!("{}", v),
            Err(e) => eprintln!("Failed to format output: {}", e),
        }
    }
}

/// A resolved profile context used to make the active environment visible to
/// the user, especially before mutating commands run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileContext {
    /// Name of the profile selected for this invocation.
    pub name: String,
    /// Environment the profile points at (e.g. `staging`, `production`).
    pub environment: String,
    /// Whether the profile was explicitly selected via `--profile`.
    pub explicit: bool,
}

impl ProfileContext {
    pub fn new(name: impl Into<String>, environment: impl Into<String>, explicit: bool) -> Self {
        Self {
            name: name.into(),
            environment: environment.into(),
            explicit,
        }
    }

    /// True when the profile targets a production environment.
    pub fn is_production(&self) -> bool {
        let env = self.environment.to_ascii_lowercase();
        env == "production" || env == "prod"
    }

    /// One-line banner describing the active profile/environment.
    pub fn banner(&self) -> String {
        let source = if self.explicit { "--profile" } else { "default" };
        format!(
            "Active profile: {} (environment: {}, source: {})",
            self.name, self.environment, source
        )
    }
}

/// Print the active profile banner so users always know which environment a
/// command is operating against.
pub fn print_profile_banner(profile: &ProfileContext) {
    eprintln!("{}", profile.banner());
}

/// Echo the target environment before a mutating command executes. Production
/// targets are highlighted so destructive commands are harder to misfire.
pub fn confirm_mutation(profile: &ProfileContext, action: &str) {
    if profile.is_production() {
        eprintln!(
            "WARNING: about to run '{}' against PRODUCTION (profile: {}, environment: {})",
            action, profile.name, profile.environment
        );
    } else {
        eprintln!(
            "Running '{}' against profile: {} (environment: {})",
            action, profile.name, profile.environment
        );
    }
}
