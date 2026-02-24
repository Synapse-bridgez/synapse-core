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
        "stellar_account"
            | "account"
            | "password"
            | "secret"
            | "token"
            | "api_key"
            | "authorization"
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
        Value::String(_s) => Value::String("****".to_string()),
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
        assert!(sanitized["user"]["account"]
            .as_str()
            .unwrap()
            .contains("****"));
        assert_eq!(sanitized["user"]["name"], "John");
    }

    #[test]
    fn test_sanitize_all_field_types() {
        let input = json!({
            "stellar_account": "GABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890",
            "account": "user_account_123",
            "password": "mypassword123",
            "secret": "topsecret",
            "token": "bearer_token_xyz",
            "api_key": "sk_live_1234567890",
            "authorization": "Bearer abc123xyz",
            "public_field": "visible_data"
        });

        let sanitized = sanitize_json(&input);
        
        assert!(sanitized["stellar_account"].as_str().unwrap().contains("****"));
        assert!(sanitized["account"].as_str().unwrap().contains("****"));
        assert!(sanitized["password"].as_str().unwrap().contains("****"));
        assert!(sanitized["secret"].as_str().unwrap().contains("****"));
        assert!(sanitized["token"].as_str().unwrap().contains("****"));
        assert!(sanitized["api_key"].as_str().unwrap().contains("****"));
        assert!(sanitized["authorization"].as_str().unwrap().contains("****"));
        assert_eq!(sanitized["public_field"], "visible_data");
    }

    #[test]
    fn test_sanitize_deeply_nested_objects() {
        let input = json!({
            "level1": {
                "level2": {
                    "level3": {
                        "password": "deep_secret",
                        "level4": {
                            "token": "nested_token",
                            "data": "public"
                        }
                    },
                    "account": "mid_account"
                },
                "public": "visible"
            }
        });

        let sanitized = sanitize_json(&input);
        
        assert!(sanitized["level1"]["level2"]["level3"]["password"].as_str().unwrap().contains("****"));
        assert!(sanitized["level1"]["level2"]["level3"]["level4"]["token"].as_str().unwrap().contains("****"));
        assert_eq!(sanitized["level1"]["level2"]["level3"]["level4"]["data"], "public");
        assert!(sanitized["level1"]["level2"]["account"].as_str().unwrap().contains("****"));
        assert_eq!(sanitized["level1"]["public"], "visible");
    }

    #[test]
    fn test_sanitize_arrays() {
        let input = json!({
            "users": [
                {"account": "user1_account", "name": "Alice"},
                {"account": "user2_account", "name": "Bob"},
                {"password": "pass123", "email": "test@example.com"}
            ],
            "tokens": ["token1", "token2", "token3"],
            "numbers": [1, 2, 3]
        });

        let sanitized = sanitize_json(&input);
        
        assert!(sanitized["users"][0]["account"].as_str().unwrap().contains("****"));
        assert_eq!(sanitized["users"][0]["name"], "Alice");
        assert!(sanitized["users"][1]["account"].as_str().unwrap().contains("****"));
        assert_eq!(sanitized["users"][1]["name"], "Bob");
        assert!(sanitized["users"][2]["password"].as_str().unwrap().contains("****"));
        assert_eq!(sanitized["users"][2]["email"], "test@example.com");
        assert_eq!(sanitized["tokens"], json!(["token1", "token2", "token3"]));
        assert_eq!(sanitized["numbers"], json!([1, 2, 3]));
    }

    #[test]
    fn test_sanitize_null_values() {
        let input = json!({
            "account": null,
            "password": null,
            "token": null,
            "normal_field": null,
            "nested": {
                "secret": null,
                "data": null
            }
        });

        let sanitized = sanitize_json(&input);
        
        assert_eq!(sanitized["account"], "****");
        assert_eq!(sanitized["password"], "****");
        assert_eq!(sanitized["token"], "****");
        assert!(sanitized["normal_field"].is_null());
        assert_eq!(sanitized["nested"]["secret"], "****");
        assert!(sanitized["nested"]["data"].is_null());
    }

    #[test]
    fn test_sanitize_large_payload_performance() {
        use std::time::Instant;

        let mut large_object = serde_json::Map::new();
        for i in 0..1000 {
            large_object.insert(format!("field_{}", i), json!(format!("value_{}", i)));
            large_object.insert(format!("account_{}", i), json!(format!("secret_account_{}", i)));
        }
        let input = Value::Object(large_object);

        let start = Instant::now();
        let sanitized = sanitize_json(&input);
        let duration = start.elapsed();

        assert!(duration.as_millis() < 1000, "Sanitization took too long: {:?}", duration);
        assert!(sanitized["account_0"].as_str().unwrap().contains("****"));
        assert_eq!(sanitized["field_0"], "value_0");
    }
}
