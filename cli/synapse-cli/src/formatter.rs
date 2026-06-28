use serde_json::Value;
use std::collections::BTreeMap;

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

pub struct Formatter;

impl Formatter {
    pub fn format_json_output<T: serde::Serialize>(
        data: &T,
        output_format: OutputFormat,
    ) -> anyhow::Result<String> {
        match output_format {
            OutputFormat::Json => {
                let json = serde_json::to_string_pretty(data)?;
                Ok(json)
            }
            OutputFormat::Table => {
                let json_value = serde_json::to_value(data)?;
                Ok(Self::format_as_table(&json_value))
            }
        }
    }

    pub fn format_bytes_output(
        data: &[u8],
        output_format: OutputFormat,
    ) -> anyhow::Result<String> {
        match output_format {
            OutputFormat::Json => {
                let text = String::from_utf8(data.to_vec())?;
                let json_value = serde_json::json!({
                    "content": text,
                    "size_bytes": data.len()
                });
                Ok(serde_json::to_string_pretty(&json_value)?)
            }
            OutputFormat::Table => {
                String::from_utf8(data.to_vec()).map_err(|e| anyhow::anyhow!(e))
            }
        }
    }

    fn format_as_table(value: &Value) -> String {
        match value {
            Value::Array(arr) => Self::format_array_as_table(arr),
            Value::Object(obj) => Self::format_object_as_table(obj),
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
    }

    fn format_object_as_table(obj: &serde_json::Map<String, Value>) -> String {
        let mut rows = Vec::new();
        let mut map: BTreeMap<&String, &Value> = obj.iter().collect();

        for (key, value) in map {
            rows.push(format!("{}: {}", key, format_value(value)));
        }

        rows.join("\n")
    }
}

fn format_value(value: &Value) -> String {
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
use serde_json::{json, Value};

/// Formats output as either table or JSON.
pub struct Formatter;

impl Formatter {
    /// Format a transaction for table display.
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

    /// Format a transaction for JSON display.
    pub fn format_transaction_json(tx: &Value) -> String {
        serde_json::to_string_pretty(tx).unwrap_or_else(|_| "{}".to_string())
    }

    /// Format output based on the requested format.
    pub fn format(format: &str, data: &Value) -> String {
        match format {
            "json" => Self::format_transaction_json(data),
            "table" | _ => Self::format_transaction_table(data),
        }
    }
}
