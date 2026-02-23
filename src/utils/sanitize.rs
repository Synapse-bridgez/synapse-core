use serde_json::Value;

/// Sanitizes sensitive fields in JSON payloads for logging
pub fn sanitize_json(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut sanitized = serde_json::Map::new();
            for (key, val) in map {
                let sanitized_val = if is_sensitive_field(key) {
                    mask_value(val)
                } else {
                    sanitize_json(val)
                };
                sanitized.insert(key.clone(), sanitized_val);
            }
            Value::Object(sanitized)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(sanitize_json).collect()),
        _ => value.clone(),
    }
}

fn is_sensitive_field(key: &str) -> bool {
    matches!(
        key.to_lowercase().as_str(),
        "stellar_account" | "account" | "password" | "secret" | "token" | "api_key" | "authorization"
    )
}

fn mask_value(value: &Value) -> Value {
    match value {
        Value::String(s) if s.len() > 8 => {
            let visible = &s[..4];
            let masked = "****";
            let end = &s[s.len() - 4..];
            Value::String(format!("{}{}{}", visible, masked, end))
        }
        Value::String(s) => Value::String("****".to_string()),
        _ => Value::String("****".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_sanitize_stellar_account() {
        let input = json!({
            "stellar_account": "GABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890",
            "amount": "100.00"
        });
        
        let sanitized = sanitize_json(&input);
        let account = sanitized["stellar_account"].as_str().unwrap();
        
        assert!(account.contains("****"));
        assert_eq!(sanitized["amount"], "100.00");
    }

    #[test]
    fn test_sanitize_nested() {
        let input = json!({
            "user": {
                "account": "secret_account_123",
                "name": "John"
            }
        });
        
        let sanitized = sanitize_json(&input);
        assert!(sanitized["user"]["account"].as_str().unwrap().contains("****"));
        assert_eq!(sanitized["user"]["name"], "John");
    }
}
