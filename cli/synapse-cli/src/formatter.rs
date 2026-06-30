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
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Array(values) => format!("[{} items]", values.len()),
        Value::Object(map) => format!("{{{} fields}}", map.len()),
    }
}
