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

/// Prompt the user for a line of input, showing `prompt` and an optional
/// `default` value that is used when the user submits an empty response.
///
/// This is the shared interactive primitive used by the `init` setup wizard so
/// that prompting behaviour (defaults, trimming, EOF handling) stays consistent
/// across the CLI.
pub fn prompt_line(prompt: &str, default: Option<&str>) -> Result<String> {
    use std::io::{self, Write};

    match default {
        Some(d) if !d.is_empty() => print!("{} [{}]: ", prompt, d),
        _ => print!("{}: ", prompt),
    }
    io::stdout().flush()?;

    let mut input = String::new();
    let read = io::stdin().read_line(&mut input)?;
    if read == 0 {
        anyhow::bail!("unexpected end of input while reading response for '{}'", prompt);
    }

    let trimmed = input.trim();
    if trimmed.is_empty() {
        if let Some(d) = default {
            return Ok(d.to_string());
        }
    }
    Ok(trimmed.to_string())
}

/// Prompt the user for a yes/no confirmation, defaulting to `default` when the
/// user submits an empty response. Used by `init` to confirm overwriting an
/// existing configuration instead of silently clobbering it.
pub fn prompt_confirm(prompt: &str, default: bool) -> Result<bool> {
    let default_hint = if default { "Y/n" } else { "y/N" };
    let answer = prompt_line(&format!("{} ({})", prompt, default_hint), None)?;
    match answer.to_ascii_lowercase().as_str() {
        "" => Ok(default),
        "y" | "yes" => Ok(true),
        "n" | "no" => Ok(false),
        _ => anyhow::bail!("please answer 'y' or 'n'"),
    }
}
