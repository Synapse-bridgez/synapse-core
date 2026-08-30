use serde::Serialize;
use serde_json::Value;

// ── OutputFormat ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Table,
    Json,
    Csv,
}

impl OutputFormat {
    pub fn from_json_flag(json: bool) -> Self {
        if json {
            Self::Json
        } else {
            Self::Table
        }
    }

    /// Extension point: to add a new output format, add a variant above and
    /// a branch here, then teach `print`/`print_one` how to render it.
    pub fn from_format_str(value: &str) -> Self {
        match value.to_ascii_lowercase().as_str() {
            "json" => Self::Json,
            "csv" => Self::Csv,
            _ => Self::Table,
        }
    }
}

/// Escape a single CSV field per RFC 4180: values containing a comma,
/// double quote, or newline are wrapped in quotes, with internal quotes
/// doubled.
pub fn csv_escape(value: &str) -> String {
    if value.contains(['"', ',', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn csv_row(fields: &[String]) -> String {
    fields
        .iter()
        .map(|f| csv_escape(f))
        .collect::<Vec<_>>()
        .join(",")
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
        OutputFormat::Csv => {
            println!(
                "{}",
                csv_row(
                    &T::headers()
                        .iter()
                        .map(|h| h.to_string())
                        .collect::<Vec<_>>()
                )
            );
            for item in items {
                println!("{}", csv_row(&item.row()));
            }
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
        OutputFormat::Csv => {
            let v = serde_json::to_value(item).unwrap_or(Value::Null);
            if let Value::Object(obj) = v {
                let headers: Vec<String> = obj.keys().cloned().collect();
                let row: Vec<String> = obj.values().map(format_cell).collect();
                println!("{}", csv_row(&headers));
                println!("{}", csv_row(&row));
            }
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
            OutputFormat::Csv => {
                let value = serde_json::to_value(data)?;
                Ok(format_csv_value(&value))
            }
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
            OutputFormat::Csv | OutputFormat::Table => {
                Ok(String::from_utf8_lossy(data).to_string())
            }
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

fn format_csv_value(value: &Value) -> String {
    match value {
        Value::Array(values) => format_csv_array(values),
        Value::Object(map) => {
            let headers: Vec<String> = map.keys().cloned().collect();
            let row: Vec<String> = map.values().map(format_cell).collect();
            format!("{}\n{}", csv_row(&headers), csv_row(&row))
        }
        other => csv_escape(&format_cell(other)),
    }
}

fn format_csv_array(values: &[Value]) -> String {
    if values.is_empty() {
        return String::new();
    }

    let Some(first) = values.iter().find_map(Value::as_object) else {
        return values
            .iter()
            .map(format_cell)
            .map(|c| csv_escape(&c))
            .collect::<Vec<_>>()
            .join("\n");
    };

    let headers = first.keys().cloned().collect::<Vec<_>>();
    let mut lines = vec![csv_row(&headers)];

    for value in values {
        if let Some(row) = value.as_object() {
            let cells: Vec<String> = headers
                .iter()
                .map(|header| row.get(header).map(format_cell).unwrap_or_default())
                .collect();
            lines.push(csv_row(&cells));
        }
    }

    lines.join("\n")
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
                // #896: s.len() is byte length; &s[..57] would panic if byte
                // offset 57 falls inside a multi-byte UTF-8 character.
                // Use char_indices to find the byte offset of the 57th char
                // boundary instead, which is always a valid slice point.
                let byte_end = s.char_indices().nth(57).map(|(i, _)| i).unwrap_or(s.len());
                format!("{}...", &s[..byte_end])
            } else {
                s.clone()
            }
        }
        Value::Array(arr) => format!("[{} items]", arr.len()),
        Value::Object(obj) => format!("{{{} fields}}", obj.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_escape_plain_value_is_unquoted() {
        assert_eq!(csv_escape("plain"), "plain");
    }

    #[test]
    fn csv_escape_quotes_field_with_comma() {
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
    }

    #[test]
    fn csv_escape_doubles_internal_quotes() {
        assert_eq!(csv_escape("say \"hi\""), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn csv_escape_quotes_field_with_newline() {
        assert_eq!(csv_escape("line1\nline2"), "\"line1\nline2\"");
    }

    #[test]
    fn from_format_str_parses_csv() {
        assert_eq!(OutputFormat::from_format_str("csv"), OutputFormat::Csv);
        assert_eq!(OutputFormat::from_format_str("CSV"), OutputFormat::Csv);
    }

    #[test]
    fn format_csv_value_escapes_embedded_commas_and_quotes() {
        let value = serde_json::json!([
            { "name": "a,b", "note": "has \"quotes\"" },
        ]);
        let csv = format_csv_value(&value);
        let mut lines = csv.lines();
        assert_eq!(lines.next().unwrap(), "name,note");
        assert_eq!(lines.next().unwrap(), "\"a,b\",\"has \"\"quotes\"\"\"");
    }
}
