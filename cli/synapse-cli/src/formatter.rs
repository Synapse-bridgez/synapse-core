use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

// ── OutputFormat ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutputFormat {
    Table,
    Json,
}

impl OutputFormat {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "json" => OutputFormat::Json,
            _ => OutputFormat::Table,
        }
    }
}

// ── TableDisplay trait ────────────────────────────────────────────────────────

/// Implement this on any type that should be displayable as a table row.
pub trait TableDisplay {
    fn headers() -> Vec<&'static str>;
    fn row(&self) -> Vec<String>;
}

// ── print / print_one helpers (used by commands::stats) ──────────────────────

/// Print a slice of `TableDisplay` items as a table or JSON array.
pub fn print<T: TableDisplay + Serialize>(items: &[T], fmt: OutputFormat) {
    match fmt {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(items)
                .unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}"));
            println!("{}", json);
        }
        OutputFormat::Table => {
            if items.is_empty() {
                println!("(no results)");
                return;
            }
            let headers = T::headers();
            // Compute column widths: max of header width and all row values
            let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
            let rows: Vec<Vec<String>> = items.iter().map(|i| i.row()).collect();
            for row in &rows {
                for (i, cell) in row.iter().enumerate() {
                    if i < widths.len() {
                        widths[i] = widths[i].max(cell.len());
                    }
                }
            }
            // Header line
            let header_line: Vec<String> = headers
                .iter()
                .enumerate()
                .map(|(i, h)| format!("{:<width$}", h, width = widths[i]))
                .collect();
            println!("{}", header_line.join("  "));
            // Separator
            let sep: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
            println!("{}", sep.join("  "));
            // Data rows
            for row in &rows {
                let cells: Vec<String> = row
                    .iter()
                    .enumerate()
                    .map(|(i, cell)| {
                        let w = widths.get(i).copied().unwrap_or(0);
                        format!("{:<width$}", cell, width = w)
                    })
                    .collect();
                println!("{}", cells.join("  "));
            }
        }
    }
}

/// Print a single `TableDisplay + Serialize` item as key-value pairs or JSON.
pub fn print_one<T: TableDisplay + Serialize>(item: &T, fmt: OutputFormat) {
    match fmt {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(item)
                .unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}"));
            println!("{}", json);
        }
        OutputFormat::Table => {
            let headers = T::headers();
            let values = item.row();
            let key_width = headers.iter().map(|h| h.len()).max().unwrap_or(0);
            for (h, v) in headers.iter().zip(values.iter()) {
                println!("{:<width$}  {}", h, v, width = key_width);
            }
        }
    }
}

// ── Formatter (used by handlers in main.rs for JSON/table generic output) ────

pub struct Formatter;

impl Formatter {
    pub fn format_json_output<T: Serialize>(
        data: &T,
        output_format: OutputFormat,
    ) -> anyhow::Result<String> {
        match output_format {
            OutputFormat::Json => Ok(serde_json::to_string_pretty(data)?),
            OutputFormat::Table => {
                let v = serde_json::to_value(data)?;
                Ok(format_value_as_table(&v))
            }
        }
    }

    pub fn format_bytes_output(data: &[u8], output_format: OutputFormat) -> anyhow::Result<String> {
        match output_format {
            OutputFormat::Json => {
                let text = String::from_utf8(data.to_vec())?;
                let v = serde_json::json!({ "content": text, "size_bytes": data.len() });
                Ok(serde_json::to_string_pretty(&v)?)
            }
            OutputFormat::Table => {
                String::from_utf8(data.to_vec()).map_err(|e| anyhow::anyhow!(e))
            }
        }
    }

    /// Format a transaction Value for display (used by `transactions get`).
    pub fn format(format: &str, data: &Value) -> String {
        match format {
            "json" => serde_json::to_string_pretty(data).unwrap_or_else(|_| "{}".to_string()),
            _ => {
                let id = data.get("id").and_then(|v| v.as_str()).unwrap_or("N/A");
                let status = data.get("status").and_then(|v| v.as_str()).unwrap_or("N/A");
                let amount = data.get("amount").and_then(|v| v.as_str()).unwrap_or("N/A");
                let asset_code = data
                    .get("asset_code")
                    .and_then(|v| v.as_str())
                    .unwrap_or("N/A");
                format!("ID\t{}\nStatus\t{}\nAmount\t{}\nAsset\t{}\n", id, status, amount, asset_code)
            }
        }
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn format_value_as_table(value: &Value) -> String {
    match value {
        Value::Array(arr) => format_array_as_table(arr),
        Value::Object(obj) => format_object_as_table(obj),
        _ => value.to_string(),
    }
}

fn format_array_as_table(arr: &[Value]) -> String {
    if arr.is_empty() {
        return "(empty)".to_string();
    }
    let mut rows = Vec::new();
    if let Value::Object(first) = &arr[0] {
        let headers: Vec<String> = first.keys().cloned().collect();
        rows.push(headers.join(" | "));
        rows.push("-".repeat(80));
        for item in arr {
            if let Value::Object(obj) = item {
                let values: Vec<String> = headers
                    .iter()
                    .map(|h| obj.get(h).map(fmt_val).unwrap_or_else(|| "-".to_string()))
                    .collect();
                rows.push(values.join(" | "));
            }
        }
    }
    rows.join("\n")
}

fn format_object_as_table(obj: &serde_json::Map<String, Value>) -> String {
    let map: BTreeMap<&String, &Value> = obj.iter().collect();
    map.iter()
        .map(|(k, v)| format!("{}: {}", k, fmt_val(v)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn fmt_val(value: &Value) -> String {
    match value {
        Value::Null => "-".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => {
            if s.len() > 50 {
                format!("{}...", &s[..47])
            } else {
                s.clone()
            }
        }
        Value::Array(arr) => format!("[{} items]", arr.len()),
        Value::Object(obj) => format!("{{{} fields}}", obj.len()),
    }
}
