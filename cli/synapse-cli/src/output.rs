use anyhow::Result;
use serde::Serialize;

pub fn render<T, F>(value: &T, json: bool, table_renderer: F) -> Result<String>
where
    T: Serialize,
    F: FnOnce(&T) -> String,
{
    if json {
        Ok(serde_json::to_string_pretty(value)?)
    } else {
        Ok(table_renderer(value))
use serde::Serialize;
use std::fmt::Display;

pub fn format_output<T: Serialize + Display>(data: T, json: bool) {
    if json {
        match serde_json::to_string(&data) {
            Ok(output) => println!("{}", output),
            Err(e) => eprintln!("Failed to serialize as JSON: {}", e),
        }
    } else {
        println!("{}", data);
    }
}
