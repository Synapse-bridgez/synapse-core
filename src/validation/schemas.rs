use jsonschema::JSONSchema;
use once_cell::sync::Lazy;
use serde_json::json;

/// Compiled JSON schemas for webhook payloads
pub struct SchemaRegistry {
    pub callback_v1: JSONSchema,
    pub webhook_v1: JSONSchema,
}

impl SchemaRegistry {
    fn new() -> Self {
        Self {
            callback_v1: JSONSchema::compile(&callback_schema_v1())
                .expect("Failed to compile callback schema"),
            webhook_v1: JSONSchema::compile(&webhook_schema_v1())
                .expect("Failed to compile webhook schema"),
        }
    }
}

/// Global schema registry with cached compiled schemas
pub static SCHEMAS: Lazy<SchemaRegistry> = Lazy::new(SchemaRegistry::new);

/// JSON schema for callback payload (v1)
fn callback_schema_v1() -> serde_json::Value {
    json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "required": ["stellar_account", "amount", "asset_code"],
        "additionalProperties": false,
        "properties": {
            "stellar_account": {
                "type": "string",
                "pattern": "^G[A-Z2-7]{55}$",
                "description": "Stellar account address"
            },
            "amount": {
                "type": "string",
                "pattern": "^[0-9]+(\\.[0-9]+)?$",
                "maxLength": 64,
                "description": "Transaction amount as decimal string"
            },
            "asset_code": {
                "type": "string",
                "pattern": "^[A-Z]{3,12}$",
                "maxLength": 12,
                "description": "Asset code (uppercase alphanumeric)"
            },
            "callback_type": {
                "type": "string",
                "maxLength": 20,
                "description": "Type of callback (e.g., deposit, withdrawal)"
            },
            "callback_status": {
                "type": "string",
                "maxLength": 20,
                "description": "Status of the callback"
            },
            "anchor_transaction_id": {
                "type": "string",
                "maxLength": 255,
                "description": "Anchor platform transaction ID"
            },
            "memo": {
                "type": "string",
                "maxLength": 255,
                "description": "Transaction memo"
            },
            "memo_type": {
                "type": "string",
                "enum": ["text", "hash", "id"],
                "description": "Type of memo"
            },
            "metadata": {
                "type": "object",
                "description": "Additional metadata as JSON object"
            }
        }
    })
}

/// JSON schema for webhook payload (v1)
fn webhook_schema_v1() -> serde_json::Value {
    json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "required": ["id"],
        "additionalProperties": false,
        "properties": {
            "id": {
                "type": "string",
                "minLength": 1,
                "maxLength": 255,
                "description": "Webhook event ID"
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_callback_schema_valid() {
        let valid = json!({
            "stellar_account": "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "amount": "100.50",
            "asset_code": "USD"
        });

        let result = SCHEMAS.callback_v1.validate(&valid);
        assert!(result.is_ok());
    }

    #[test]
    fn test_callback_schema_with_optional_fields() {
        let valid = json!({
            "stellar_account": "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "amount": "100.50",
            "asset_code": "USD",
            "callback_type": "deposit",
            "callback_status": "completed",
            "anchor_transaction_id": "anchor-123",
            "memo": "test memo",
            "memo_type": "text",
            "metadata": {"key": "value"}
        });

        let result = SCHEMAS.callback_v1.validate(&valid);
        assert!(result.is_ok());
    }

    #[test]
    fn test_callback_schema_missing_required() {
        let invalid = json!({
            "amount": "100.50",
            "asset_code": "USD"
        });

        let result = SCHEMAS.callback_v1.validate(&invalid);
        assert!(result.is_err());
    }

    #[test]
    fn test_callback_schema_invalid_stellar_account() {
        let invalid = json!({
            "stellar_account": "INVALID",
            "amount": "100.50",
            "asset_code": "USD"
        });

        let result = SCHEMAS.callback_v1.validate(&invalid);
        assert!(result.is_err());
    }

    #[test]
    fn test_callback_schema_invalid_amount() {
        let invalid = json!({
            "stellar_account": "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "amount": "-100.50",
            "asset_code": "USD"
        });

        let result = SCHEMAS.callback_v1.validate(&invalid);
        assert!(result.is_err());
    }

    #[test]
    fn test_callback_schema_invalid_asset_code() {
        let invalid = json!({
            "stellar_account": "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "amount": "100.50",
            "asset_code": "usd"
        });

        let result = SCHEMAS.callback_v1.validate(&invalid);
        assert!(result.is_err());
    }

    #[test]
    fn test_callback_schema_additional_properties() {
        let invalid = json!({
            "stellar_account": "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "amount": "100.50",
            "asset_code": "USD",
            "unknown_field": "value"
        });

        let result = SCHEMAS.callback_v1.validate(&invalid);
        assert!(result.is_err());
    }

    #[test]
    fn test_callback_schema_invalid_memo_type() {
        let invalid = json!({
            "stellar_account": "GAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "amount": "100.50",
            "asset_code": "USD",
            "memo_type": "invalid"
        });

        let result = SCHEMAS.callback_v1.validate(&invalid);
        assert!(result.is_err());
    }

    #[test]
    fn test_webhook_schema_valid() {
        let valid = json!({
            "id": "webhook-123"
        });

        let result = SCHEMAS.webhook_v1.validate(&valid);
        assert!(result.is_ok());
    }

    #[test]
    fn test_webhook_schema_missing_id() {
        let invalid = json!({});

        let result = SCHEMAS.webhook_v1.validate(&invalid);
        assert!(result.is_err());
    }

    #[test]
    fn test_webhook_schema_additional_properties() {
        let invalid = json!({
            "id": "webhook-123",
            "extra": "field"
        });

        let result = SCHEMAS.webhook_v1.validate(&invalid);
        assert!(result.is_err());
    }
}
