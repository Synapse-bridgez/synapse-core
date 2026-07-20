use serde::Serialize;
use serde_json::Value;

// ── OutputFormat ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Table,
    Json,
}

impl OutputFormat {
    pub fn from_json_flag(json: bool) -> Self {
        if json {
            Self::Json
        } else {
            Self::Table
        }
    }

    pub fn from_format_str(value: &str) -> Self {
        match value.to_ascii_lowercase().as_str() {
            "json" => Self::Json,
            _ => Self::Table,
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
            println!(
                "{}",
                serde_json::to_string_pretty(items).unwrap_or_else(|_| "[]".into())
            );
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

            let header_line: Vec<String> = headers
                .iter()
                .zip(widths.iter())
                .map(|(h, w)| format!("{:<width$}", h, width = w))
                .collect();
            println!("{}", header_line.join("  "));

            let sep: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
            println!("{}", sep.join("  "));

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
            println!(
                "{}",
                serde_json::to_string_pretty(item).unwrap_or_else(|_| "{}".into())
            );
        }
        OutputFormat::Table => {
            let v = serde_json::to_value(item).unwrap_or(Value::Null);
            println!("{}", format_value_as_kv(&v));
        }
    }
}

// ── Formatter (legacy helpers used by settlements/transactions/admin handlers) ─

pub struct Formatter;

impl Formatter {
    pub fn format_json_output<T: Serialize>(
        data: &T,
        output_format: OutputFormat,
    ) -> anyhow::Result<String> {
        match output_format {
            OutputFormat::Json => Ok(serde_json::to_string_pretty(data)?),
            OutputFormat::Table => {
                let value = serde_json::to_value(data)?;
                Ok(format_table_value(&value))
            }
        }
    }

    pub fn format_bytes_output(data: &[u8], output_format: OutputFormat) -> anyhow::Result<String> {
        match output_format {
            OutputFormat::Json => {
                let text = String::from_utf8_lossy(data).to_string();
                let json_value = serde_json::json!({ "content": text, "size_bytes": data.len() });
                Ok(serde_json::to_string_pretty(&json_value)?)
            }
            OutputFormat::Table => Ok(String::from_utf8_lossy(data).to_string()),
        }
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn format_value_as_kv(value: &Value) -> String {
    match value {
        Value::Object(obj) => obj
            .iter()
            .map(|(k, v)| format!("{}: {}", k, format_cell(v)))
            .collect::<Vec<_>>()
            .join("\n"),
        other => format_cell(other),
    }
}

fn format_table_value(value: &Value) -> String {
    match value {
        Value::Array(values) => format_array(values),
        Value::Object(map) => map
            .iter()
            .map(|(key, value)| format!("{key}: {}", format_cell(value)))
            .collect::<Vec<_>>()
            .join("\n"),
        other => format_cell(other),
    }
}

fn format_array(values: &[Value]) -> String {
    if values.is_empty() {
        return "(empty)".to_string();
    }

    let Some(first) = values.iter().find_map(Value::as_object) else {
        return values
            .iter()
            .map(format_cell)
            .collect::<Vec<_>>()
            .join("\n");
    };

    let headers = first.keys().cloned().collect::<Vec<_>>();
    let mut lines = vec![headers.join(" | "), "-".repeat(80)];

    for value in values {
        if let Some(row) = value.as_object() {
            lines.push(
                headers
                    .iter()
                    .map(|header| {
                        row.get(header)
                            .map(format_cell)
                            .unwrap_or_else(|| "-".into())
                    })
                    .collect::<Vec<_>>()
                    .join(" | "),
            );
        }
    }

    lines.join("\n")
}

fn format_cell(value: &Value) -> String {
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
