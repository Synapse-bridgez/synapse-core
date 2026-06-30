use anyhow::Result;
use serde::Serialize;

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
