use serde::Serialize;
use serde_json::Value;

// ── OutputFormat ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
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

    pub fn from_str(value: &str) -> Self {
        match value.to_ascii_lowercase().as_str() {
            "json" => Self::Json,
            _ => Self::Table,
        }
    }
}

// ── TableDisplay trait ────────────────────────────────────────────────────────
// Commands implement this trait so that the shared `print` / `print_one`
// functions can render either a table or pretty-printed JSON without any
// command-specific logic in the formatter itself.

pub trait TableDisplay: Serialize {
    /// Column headers for the table view.
    fn headers() -> Vec<&'static str>;
    /// One table row, matching the order of `headers()`.
    fn row(&self) -> Vec<String>;
}

// ── print / print_one ─────────────────────────────────────────────────────────

/// Print a list of items as a table or JSON array.
pub fn print<T: TableDisplay>(items: &[T], fmt: OutputFormat) {
    match fmt {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(items)
                .unwrap_or_else(|e| format!("{{\"error\":\"{}\"}}", e));
            println!("{}", json);
        }
        OutputFormat::Table => {
            let headers = T::headers();
            // Build column widths from headers and data.
            let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
            let rows: Vec<Vec<String>> = items.iter().map(|item| item.row()).collect();
            for row in &rows {
                for (i, cell) in row.iter().enumerate() {
                    if i < widths.len() {
                        widths[i] = widths[i].max(cell.len());
                    }
                }
            }

            // Header row.
            let header_line: Vec<String> = headers
                .iter()
                .enumerate()
                .map(|(i, h)| format!("{:<width$}", h, width = widths[i]))
                .collect();
            println!("{}", header_line.join("  "));

            // Separator.
            let sep: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
            println!("{}", sep.join("  "));

            // Data rows.
            for row in &rows {
                let line: Vec<String> = row
                    .iter()
                    .enumerate()
                    .map(|(i, cell)| {
                        let w = widths.get(i).copied().unwrap_or(cell.len());
                        format!("{:<width$}", cell, width = w)
                    })
                    .collect();
                println!("{}", line.join("  "));
            }

            if items.is_empty() {
                println!("(no results)");
            }
        }
    }
}

/// Print a single item as a key-value table or JSON object.
pub fn print_one<T: TableDisplay>(item: &T, fmt: OutputFormat) {
    match fmt {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(item)
                .unwrap_or_else(|e| format!("{{\"error\":\"{}\"}}", e));
            println!("{}", json);
        }
        OutputFormat::Table => {
            let headers = T::headers();
            let row = item.row();
            let key_width = headers.iter().map(|h| h.len()).max().unwrap_or(0);
            for (header, value) in headers.iter().zip(row.iter()) {
                println!("{:<width$}  {}", header, value, width = key_width);
            }
        }
    }
}

// ── Formatter (legacy helpers used by transactions/settlements handlers) ──────

pub struct Formatter;

impl Formatter {
    pub fn format_json_output<T: serde::Serialize>(
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
                let text = String::from_utf8(data.to_vec())?;
                Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "content": text,
                    "size_bytes": data.len()
                }))?)
            }
            OutputFormat::Table => Ok(String::from_utf8(data.to_vec())?),
        }
    }
}

    /// Format a transaction `Value` for human-readable table display.
    pub fn format_transaction_table(tx: &Value) -> String {
        let id = tx.get("id").and_then(|v| v.as_str()).unwrap_or("N/A");
        let status = tx.get("status").and_then(|v| v.as_str()).unwrap_or("N/A");
        let amount = tx.get("amount").and_then(|v| v.as_str()).unwrap_or("N/A");
        let asset_code = tx
            .get("asset_code")
            .and_then(|v| v.as_str())
            .unwrap_or("N/A");
        format!(
            "ID\t{}\nStatus\t{}\nAmount\t{}\nAsset\t{}\n",
            id, status, amount, asset_code
        )
    }

    /// Format a transaction `Value` as pretty-printed JSON.
    pub fn format_transaction_json(tx: &Value) -> String {
        serde_json::to_string_pretty(tx).unwrap_or_else(|_| "{}".to_string())
    }

    /// Format a `Value` based on a format string (`"json"` or anything else → table).
    pub fn format(format: &str, data: &Value) -> String {
        match format {
            "json" => Self::format_transaction_json(data),
            _ => Self::format_transaction_table(data),
        }
    }

    fn format_as_table(value: &Value) -> String {
        match value {
            Value::Array(arr) => Self::format_array_as_table(arr),
            Value::Object(obj) => Self::format_object_as_table(obj),
            _ => value.to_string(),
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
                        .map(|h| {
                            obj.get(h)
                                .map(|v| format_value(v))
                                .unwrap_or_else(|| "-".to_string())
                        })
                        .collect();
                    rows.push(values.join(" | "));
                }
            }
        }

        rows.join("\n")
fn format_array(values: &[Value]) -> String {
    if values.is_empty() {
        return "(empty)".to_string();
    }

    fn format_object_as_table(obj: &serde_json::Map<String, Value>) -> String {
        let mut rows = Vec::new();
        let map: BTreeMap<&String, &Value> = obj.iter().collect();

        for (key, value) in map {
            rows.push(format!("{}: {}", key, format_value(value)));
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
        rows.join("\n")
    }

    lines.join("\n")
}

fn format_cell(value: &Value) -> String {
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

/// Trait for types that can render themselves as a table row.
pub trait TableDisplay: serde::Serialize {
    fn headers() -> Vec<&'static str>;
    fn row(&self) -> Vec<String>;
}

/// Print a slice of table-displayable items to stdout.
pub fn print<T: TableDisplay>(items: &[T], fmt: OutputFormat) {
    match fmt {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(items).unwrap_or_else(|_| "[]".to_string());
            println!("{}", json);
        }
        OutputFormat::Table => {
            if items.is_empty() {
                println!("(empty)");
                return;
            }
            let headers = T::headers();
            println!("{}", headers.join(" | "));
            println!("{}", "-".repeat(80));
            for item in items {
                println!("{}", item.row().join(" | "));
            }
        }
    }
}

/// Print a single serializable item to stdout.
pub fn print_one<T: serde::Serialize>(item: &T, fmt: OutputFormat) {
    match fmt {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(item).unwrap_or_else(|_| "{}".to_string());
            println!("{}", json);
        }
        OutputFormat::Table => {
            let v = serde_json::to_value(item).unwrap_or(Value::Null);
            println!("{}", Formatter::format_as_table(&v));
        }
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Array(values) => format!("[{} items]", values.len()),
        Value::Object(map) => format!("{{{} fields}}", map.len()),
    }
}
