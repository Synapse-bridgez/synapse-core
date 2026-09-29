use serde_json::Value;
use regex::Regex;

/// Per-tenant memo redaction configuration
#[derive(Debug, Clone)]
pub struct MemoRedactionConfig {
    /// List of regex patterns to match sensitive content in memos
    pub patterns: Vec<String>,
    /// Replacement string for matched content (e.g., "***", "[REDACTED]")
    pub replacement: String,
    /// Whether redaction is enabled
    pub enabled: bool,
}

impl Default for MemoRedactionConfig {
    fn default() -> Self {
        Self {
            patterns: vec![],
            replacement: "[REDACTED]".to_string(),
            enabled: false,
        }
    }
}

impl MemoRedactionConfig {
    pub fn new(patterns: Vec<String>, replacement: String) -> Result<Self, String> {
        // Validate all patterns are valid regex before constructing
        for pattern in &patterns {
            Regex::new(pattern).map_err(|e| format!("Invalid regex pattern '{}': {}", pattern, e))?;
        }
        Ok(Self {
            patterns,
            replacement,
            enabled: !patterns.is_empty(),
        })
    }

    /// Apply redaction to a memo string
    pub fn apply(&self, memo: &str) -> String {
        if !self.enabled {
            return memo.to_string();
        }

        let mut result = memo.to_string();
        for pattern in &self.patterns {
            if let Ok(re) = Regex::new(pattern) {
                result = re.replace_all(&result, self.replacement.clone()).to_string();
            }
        }
        result
    }
}

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
    let key_lower = key.to_lowercase();
    // Exact matches
    if matches!(
        key_lower.as_str(),
        "stellar_account"
            | "account"
            | "password"
            | "secret"
            | "token"
            | "api_key"
            | "authorization"
    ) {
        return true;
    }

    // Pattern matches for fields like "account_0", "user_password", etc.
    // But not "accounts" or "tokens" (plurals)
    key_lower.starts_with("account_")
        || key_lower.starts_with("password_")
        || key_lower.starts_with("secret_")
        || key_lower.starts_with("token_")
        || key_lower.ends_with("_account")
        || key_lower.ends_with("_password")
        || key_lower.ends_with("_secret")
        || key_lower.ends_with("_token")
}

fn mask_value(value: &Value) -> Value {
    match value {
        Value::String(s) if s.len() > 8 => {
            let visible = &s[..4];
            let masked = "****";
            let end = &s[s.len() - 4..];
            Value::String(format!("{visible}{masked}{end}"))
        }
        Value::String(_s) => Value::String("****".to_string()),
        _ => Value::String("****".to_string()),
    }
}

/// Sanitize and redact memo content in JSON payloads
pub fn redact_memo_json(value: &Value, redaction_config: &MemoRedactionConfig) -> Value {
    if !redaction_config.enabled {
        return value.clone();
    }

    match value {
        Value::Object(map) => {
            let mut sanitized = serde_json::Map::new();
            for (key, val) in map {
                let sanitized_val = if key.to_lowercase() == "memo" {
                    if let Value::String(memo) = val {
                        let redacted = redaction_config.apply(memo);
                        Value::String(redacted)
                    } else {
                        val.clone()
                    }
                } else {
                    redact_memo_json(val, redaction_config)
                };
                sanitized.insert(key.clone(), sanitized_val);
            }
            Value::Object(sanitized)
        }
        Value::Array(arr) => {
            Value::Array(arr.iter().map(|v| redact_memo_json(v, redaction_config)).collect())
        }
        _ => value.clone(),
    }
}

/// Redact memo content in a string if it's memo field
pub fn redact_memo_string(field_name: &str, value: &str, redaction_config: &MemoRedactionConfig) -> String {
    if !redaction_config.enabled || field_name.to_lowercase() != "memo" {
        return value.to_string();
    }
    redaction_config.apply(value)
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

        assert!(sanitized["stellar_account"]
            .as_str()
            .unwrap()
            .contains("****"));
        assert!(sanitized["account"].as_str().unwrap().contains("****"));
        assert!(sanitized["password"].as_str().unwrap().contains("****"));
        assert!(sanitized["secret"].as_str().unwrap().contains("****"));
        assert!(sanitized["token"].as_str().unwrap().contains("****"));
        assert!(sanitized["api_key"].as_str().unwrap().contains("****"));
        assert!(sanitized["authorization"]
            .as_str()
            .unwrap()
            .contains("****"));
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

        assert!(sanitized["level1"]["level2"]["level3"]["password"]
            .as_str()
            .unwrap()
            .contains("****"));
        assert!(sanitized["level1"]["level2"]["level3"]["level4"]["token"]
            .as_str()
            .unwrap()
            .contains("****"));
        assert_eq!(
            sanitized["level1"]["level2"]["level3"]["level4"]["data"],
            "public"
        );
        assert!(sanitized["level1"]["level2"]["account"]
            .as_str()
            .unwrap()
            .contains("****"));
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

        assert!(sanitized["users"][0]["account"]
            .as_str()
            .unwrap()
            .contains("****"));
        assert_eq!(sanitized["users"][0]["name"], "Alice");
        assert!(sanitized["users"][1]["account"]
            .as_str()
            .unwrap()
            .contains("****"));
        assert_eq!(sanitized["users"][1]["name"], "Bob");
        assert!(sanitized["users"][2]["password"]
            .as_str()
            .unwrap()
            .contains("****"));
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
            large_object.insert(
                format!("account_{}", i),
                json!(format!("secret_account_{}", i)),
            );
        }
        let input = Value::Object(large_object);

        let start = Instant::now();
        let sanitized = sanitize_json(&input);
        let duration = start.elapsed();

        assert!(
            duration.as_millis() < 1000,
            "Sanitization took too long: {:?}",
            duration
        );
        assert!(sanitized["account_0"].as_str().unwrap().contains("****"));
        assert_eq!(sanitized["field_0"], "value_0");
    }

    #[test]
    fn test_memo_redaction_config_creation() {
        let patterns = vec!["\\d{3}-\\d{2}-\\d{4}".to_string(), "SSN:\\s*\\d+".to_string()];
        let config = MemoRedactionConfig::new(patterns, "[REDACTED]".to_string())
            .expect("Config creation failed");
        assert!(config.enabled);
        assert_eq!(config.patterns.len(), 2);
    }

    #[test]
    fn test_memo_redaction_disabled_by_default() {
        let config = MemoRedactionConfig::default();
        assert!(!config.enabled);
        assert_eq!(config.apply("secret data"), "secret data");
    }

    #[test]
    fn test_memo_redaction_apply_single_pattern() {
        let config = MemoRedactionConfig::new(
            vec!["secret".to_string()],
            "[REDACTED]".to_string(),
        ).expect("Config creation failed");

        assert_eq!(config.apply("this is a secret"), "this is a [REDACTED]");
    }

    #[test]
    fn test_memo_redaction_apply_multiple_patterns() {
        let config = MemoRedactionConfig::new(
            vec![
                "\\d{3}-\\d{2}-\\d{4}".to_string(),
                "SSN:\\s*\\d+".to_string(),
            ],
            "[REDACTED]".to_string(),
        ).expect("Config creation failed");

        let input = "Customer SSN: 123456789 with ID 123-45-6789";
        let output = config.apply(input);
        assert_eq!(output, "Customer [REDACTED]: [REDACTED] with ID [REDACTED]");
    }

    #[test]
    fn test_redact_memo_json_disabled() {
        let config = MemoRedactionConfig::default();
        let input = json!({
            "memo": "secret memo content"
        });

        let output = redact_memo_json(&input, &config);
        assert_eq!(output["memo"], "secret memo content");
    }

    #[test]
    fn test_redact_memo_json_enabled() {
        let config = MemoRedactionConfig::new(
            vec!["secret".to_string()],
            "[REDACTED]".to_string(),
        ).expect("Config creation failed");

        let input = json!({
            "memo": "this is a secret memo",
            "other_field": "this is a secret field"
        });

        let output = redact_memo_json(&input, &config);
        assert_eq!(output["memo"], "this is a [REDACTED] memo");
        // Other fields should not be redacted (memo-specific)
        assert_eq!(output["other_field"], "this is a secret field");
    }

    #[test]
    fn test_redact_memo_string() {
        let config = MemoRedactionConfig::new(
            vec!["customer_id:\\s*\\d+".to_string()],
            "[REDACTED]".to_string(),
        ).expect("Config creation failed");

        let memo = "Payment for customer_id: 12345 received";
        let output = redact_memo_string("memo", memo, &config);
        assert_eq!(output, "Payment for [REDACTED] received");

        // Non-memo fields should not be redacted
        let output = redact_memo_string("description", memo, &config);
        assert_eq!(output, memo);
    }

    #[test]
    fn test_memo_redaction_parity_export_webhook() {
        let config = MemoRedactionConfig::new(
            vec!["PII:\\s*[A-Z0-9]+".to_string()],
            "[REDACTED]".to_string(),
        ).expect("Config creation failed");

        let memo = "User data: PII: SENSITIVE123 and other info";

        // Simulate export path (string-based)
        let export_result = redact_memo_string("memo", memo, &config);

        // Simulate webhook path (JSON-based)
        let json_input = json!({"memo": memo});
        let webhook_result = redact_memo_json(&json_input, &config);

        // Both paths should produce the same result
        assert_eq!(export_result, webhook_result["memo"].as_str().unwrap());
    }
}

#[cfg(test)]
mod property_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// sanitize_json must be idempotent: applying it twice gives the same result.
        #[test]
        fn prop_sanitize_json_idempotent(
            key in "[a-zA-Z_]{1,20}",
            value in "[a-zA-Z0-9 ]{0,100}",
        ) {
            let input = serde_json::json!({ key: value });
            let once = sanitize_json(&input);
            let twice = sanitize_json(&once);
            prop_assert_eq!(once, twice, "sanitize_json is not idempotent");
        }

        /// Non-sensitive fields must never be masked.
        #[test]
        fn prop_non_sensitive_fields_not_masked(
            // Use field names that are clearly not sensitive
            key in "data|info|result|value|name|count|total",
            value in "[a-zA-Z0-9]{5,20}",
        ) {
            let input = serde_json::json!({ key.clone(): value.clone() });
            let sanitized = sanitize_json(&input);
            prop_assert_eq!(
                sanitized[&key].as_str().unwrap_or(""),
                value.as_str(),
                "Non-sensitive field '{}' was unexpectedly masked",
                key
            );
        }

        /// Sensitive fields must always be masked.
        #[test]
        fn prop_sensitive_fields_always_masked(
            value in "[a-zA-Z0-9]{5,50}",
        ) {
            for key in &["password", "secret", "token", "api_key", "account", "authorization"] {
                let input = serde_json::json!({ *key: value.clone() });
                let sanitized = sanitize_json(&input);
                let sanitized_val = sanitized[key].as_str().unwrap_or("");
                prop_assert!(
                    sanitized_val.contains("****"),
                    "Sensitive field '{}' was not masked, got: {}",
                    key,
                    sanitized_val
                );
            }
        }

        /// sanitize_json must not panic on deeply nested or large inputs.
        #[test]
        fn prop_sanitize_json_handles_arrays(
            values in prop::collection::vec("[a-zA-Z0-9]{1,20}", 0..50),
        ) {
            let input = serde_json::Value::Array(
                values.iter().map(|v| serde_json::json!(v)).collect()
            );
            let _ = sanitize_json(&input);
        }
    }
}
