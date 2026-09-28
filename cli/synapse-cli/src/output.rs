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

#[cfg(test)]
mod golden_tests {
    use super::*;
    use crate::formatter::{Formatter, OutputFormat};
    use serde::Serialize;
    use std::fs;
    use std::path::PathBuf;

    /// Representative row shape used across the golden fixtures.
    #[derive(Serialize)]
    struct Row {
        id: u32,
        name: String,
        status: String,
    }

    fn sample_rows() -> Vec<Row> {
        vec![
            Row { id: 1, name: "alpha".into(), status: "active".into() },
            Row { id: 2, name: "beta".into(), status: "inactive".into() },
        ]
    }

    fn empty_rows() -> Vec<Row> {
        Vec::new()
    }

    fn long_value_rows() -> Vec<Row> {
        vec![Row {
            id: 3,
            name: "x".repeat(200),
            status: "active".into(),
        }]
    }

    fn unicode_rows() -> Vec<Row> {
        vec![Row {
            id: 4,
            name: "caf\u{e9} \u{1f680} \"quoted\" \\slash\t tab".into(),
            status: "\u{4f60}\u{597d}".into(),
        }]
    }

    fn golden_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
    }

    /// Compare `actual` against the checked-in golden file at `name`. When the
    /// `UPDATE_GOLDEN` env var is set, the fixture is (re)written instead so
    /// intentional formatting changes require an explicit, reviewable update.
    fn assert_golden(name: &str, actual: &str) {
        let path = golden_dir().join(name);
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            fs::create_dir_all(golden_dir()).expect("create golden dir");
            fs::write(&path, actual).expect("write golden file");
            return;
        }
        let expected = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("missing golden file {}: {}", path.display(), e));
        assert_eq!(
            expected, actual,
            "golden mismatch for {}; run with UPDATE_GOLDEN=1 to regenerate",
            name
        );
    }

    fn render_json<T: Serialize>(value: &T) -> String {
        Formatter::format_json_output(value, OutputFormat::Json).expect("json render")
    }

    fn render_yaml<T: Serialize>(value: &T) -> String {
        Formatter::format_json_output(value, OutputFormat::Yaml).expect("yaml render")
    }

    fn render_table<T: Serialize>(value: &T) -> String {
        Formatter::format_table_output(value).expect("table render")
    }

    #[test]
    fn golden_json_representative() {
        assert_golden("json_representative.json", &render_json(&sample_rows()));
    }

    #[test]
    fn golden_json_empty() {
        assert_golden("json_empty.json", &render_json(&empty_rows()));
    }

    #[test]
    fn golden_json_long_values() {
        assert_golden("json_long_values.json", &render_json(&long_value_rows()));
    }

    #[test]
    fn golden_json_unicode_escaping() {
        assert_golden("json_unicode.json", &render_json(&unicode_rows()));
    }

    #[test]
    fn golden_yaml_representative() {
        assert_golden("yaml_representative.yaml", &render_yaml(&sample_rows()));
    }

    #[test]
    fn golden_yaml_empty() {
        assert_golden("yaml_empty.yaml", &render_yaml(&empty_rows()));
    }

    #[test]
    fn golden_yaml_long_values() {
        assert_golden("yaml_long_values.yaml", &render_yaml(&long_value_rows()));
    }

    #[test]
    fn golden_yaml_unicode_escaping() {
        assert_golden("yaml_unicode.yaml", &render_yaml(&unicode_rows()));
    }

    #[test]
    fn golden_table_representative() {
        assert_golden("table_representative.txt", &render_table(&sample_rows()));
    }

    #[test]
    fn golden_table_empty() {
        assert_golden("table_empty.txt", &render_table(&empty_rows()));
    }

    #[test]
    fn golden_table_long_values() {
        assert_golden("table_long_values.txt", &render_table(&long_value_rows()));
    }

    #[test]
    fn golden_table_unicode_escaping() {
        assert_golden("table_unicode.txt", &render_table(&unicode_rows()));
    }
}
