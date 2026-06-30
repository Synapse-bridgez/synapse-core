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
