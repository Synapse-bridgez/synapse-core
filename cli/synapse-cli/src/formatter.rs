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

/// Implement this for any type that can be rendered as a CLI table row.
pub trait TableDisplay {
    fn headers() -> Vec<&'static str>;
    fn row(&self) -> Vec<String>;
}

// ── Top-level print helpers ───────────────────────────────────────────────────

/// Print a list of items as a table or JSON array.
pub fn print<T>(items: &[T], fmt: OutputFormat)
where
    T: TableDisplay + Serialize,
{
    match fmt {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(items).unwrap_or_else(|_| "[]".into()));
        }
        OutputFormat::Table => {
            if items.is_empty() {
                println!("(no results)");
                return;
            }
            let headers = T::headers();
            let widths: Vec<usize> = headers
                .iter()
                .enumerate()
                .map(|(i, h)| {
                    items
                        .iter()
                        .map(|item| item.row().get(i).map(|s| s.len()).unwrap_or(0))
                        .max()
                        .unwrap_or(0)
                        .max(h.len())
                })
                .collect();

            // Header row
            let header_line: Vec<String> = headers
                .iter()
                .zip(widths.iter())
                .map(|(h, w)| format!("{:<width$}", h, width = w))
                .collect();
            println!("{}", header_line.join("  "));

            // Separator
            let sep: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
            println!("{}", sep.join("  "));

            // Data rows
            for item in items {
                let row = item.row();
                let cells: Vec<String> = row
                    .iter()
                    .zip(widths.iter())
                    .map(|(v, w)| format!("{:<width$}", v, width = w))
                    .collect();
                println!("{}", cells.join("  "));
            }
        }
    }
}

/// Print a single struct as a key-value table or JSON object.
pub fn print_one<T: Serialize>(item: &T, fmt: OutputFormat) {
    match fmt {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(item).unwrap_or_else(|_| "{}".into()));
        }
        OutputFormat::Table => {
            let v = serde_json::to_value(item).unwrap_or(Value::Null);
            println!("{}", format_value_as_kv(&v));
        }
    }
}

// ── Formatter struct (legacy / settlements / transactions usage) ──────────────

pub struct Formatter;

impl Formatter {
    pub fn format_json_output<T: Serialize>(
        data: &T,
        output_format: OutputFormat,
    ) -> anyhow::Result<String> {
        match output_format {
            OutputFormat::Json => Ok(serde_json::to_string_pretty(data)?),
            OutputFormat::Table => {
                let json_value = serde_json::to_value(data)?;
                Ok(format_value_as_table(&json_value))
            }
        }
    }

    pub fn format_bytes_output(data: &[u8], output_format: OutputFormat) -> anyhow::Result<String> {
        match output_format {
            OutputFormat::Json => {
                let text = String::from_utf8(data.to_vec())?;
                let json_value = serde_json::json!({ "content": text, "size_bytes": data.len() });
                Ok(serde_json::to_string_pretty(&json_value)?)
            }
            OutputFormat::Table => {
                String::from_utf8(data.to_vec()).map_err(|e| anyhow::anyhow!(e))
            }
        }
    }

    // For transactions get (table mode)
    pub fn format_transaction_table(tx: &Value) -> String {
        let id = tx.get("id").and_then(|v| v.as_str()).unwrap_or("N/A");
        let status = tx.get("status").and_then(|v| v.as_str()).unwrap_or("N/A");
        let amount = tx.get("amount").and_then(|v| v.as_str()).unwrap_or("N/A");
        let asset_code = tx.get("asset_code").and_then(|v| v.as_str()).unwrap_or("N/A");
        format!("ID\t{}\nStatus\t{}\nAmount\t{}\nAsset\t{}\n", id, status, amount, asset_code)
    }

    pub fn format_transaction_json(tx: &Value) -> String {
        serde_json::to_string_pretty(tx).unwrap_or_else(|_| "{}".to_string())
    }

    pub fn format(format: &str, data: &Value) -> String {
        match format {
            "json" => Self::format_transaction_json(data),
            _ => Self::format_transaction_table(data),
        }
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn format_value_as_table(value: &Value) -> String {
    match value {
        Value::Array(arr) => format_array_as_table(arr),
        Value::Object(obj) => format_object_as_kv(obj),
        _ => value.to_string(),
    }
}

fn format_value_as_kv(value: &Value) -> String {
    match value {
        Value::Object(obj) => format_object_as_kv(obj),
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
                    .map(|h| obj.get(h).map(display_scalar).unwrap_or_else(|| "-".into()))
                    .collect();
                rows.push(values.join(" | "));
            }
        }
    }
    rows.join("\n")
}

fn format_object_as_kv(obj: &serde_json::Map<String, Value>) -> String {
    let map: BTreeMap<&String, &Value> = obj.iter().collect();
    map.iter()
        .map(|(k, v)| format!("{}: {}", k, display_scalar(v)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn display_scalar(value: &Value) -> String {
    match value {
        Value::Null => "-".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => {
            if s.len() > 60 {
                format!("{}...", &s[..57])
            } else {
                s.clone()
            }
        }
        Value::Array(arr) => format!("[{} items]", arr.len()),
        Value::Object(obj) => format!("{{{} fields}}", obj.len()),
    }
}
