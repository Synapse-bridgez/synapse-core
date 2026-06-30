use serde_json::Value;

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
