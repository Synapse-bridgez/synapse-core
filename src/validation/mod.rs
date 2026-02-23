use serde::Deserialize;
use sqlx::types::BigDecimal;
use std::fmt;

pub const STELLAR_ACCOUNT_LEN: usize = 56;
pub const ASSET_CODE_MAX_LEN: usize = 12;
pub const ANCHOR_TRANSACTION_ID_MAX_LEN: usize = 255;
pub const CALLBACK_TYPE_MAX_LEN: usize = 20;
pub const CALLBACK_STATUS_MAX_LEN: usize = 20;
pub const AMOUNT_INPUT_MAX_LEN: usize = 64;
pub const ALLOWED_ASSET_CODES: &[&str] = &["USD"];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrictPayload<T> {
    #[serde(flatten)]
    pub data: T,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub field: &'static str,
    pub message: String,
}

impl ValidationError {
    pub fn new(field: &'static str, message: impl Into<String>) -> Self {
        Self {
            field,
            message: message.into(),
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

impl std::error::Error for ValidationError {}

pub type ValidationResult = Result<(), ValidationError>;

pub fn sanitize_string(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_control())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn validate_required(field: &'static str, value: &str) -> ValidationResult {
    if value.trim().is_empty() {
        return Err(ValidationError::new(field, "must not be empty"));
    }

    Ok(())
}

pub fn validate_max_len(field: &'static str, value: &str, max_len: usize) -> ValidationResult {
    if value.len() > max_len {
        return Err(ValidationError::new(
            field,
            format!("must be at most {} characters", max_len),
        ));
    }

    Ok(())
}

pub fn validate_enum(field: &'static str, value: &str, allowed: &[&str]) -> ValidationResult {
    if allowed.iter().all(|candidate| value != *candidate) {
        return Err(ValidationError::new(
            field,
            format!("must be one of: {}", allowed.join(", ")),
        ));
    }

    Ok(())
}

pub fn validate_stellar_address(stellar_address: &str) -> ValidationResult {
    let stellar_address = sanitize_string(stellar_address);
    validate_required("stellar_address", &stellar_address)?;

    if stellar_address.len() != STELLAR_ACCOUNT_LEN {
        return Err(ValidationError::new(
            "stellar_address",
            format!("must be exactly {} characters", STELLAR_ACCOUNT_LEN),
        ));
    }

    if !stellar_address.starts_with('G') {
        return Err(ValidationError::new(
            "stellar_address",
            "must start with 'G'",
        ));
    }

    if !stellar_address
        .chars()
        .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
    {
        return Err(ValidationError::new(
            "stellar_address",
            "must contain only uppercase letters and digits",
        ));
    }

    Ok(())
}

pub fn validate_stellar_account(account: &str) -> ValidationResult {
    validate_stellar_address(account)
}

pub fn validate_asset_code(asset_code: &str) -> ValidationResult {
    let asset_code = sanitize_string(asset_code);
    validate_required("asset_code", &asset_code)?;
    validate_max_len("asset_code", &asset_code, ASSET_CODE_MAX_LEN)?;

    if !asset_code
        .chars()
        .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
    {
        return Err(ValidationError::new(
            "asset_code",
            "must contain only uppercase letters and digits",
        ));
    }

    validate_enum("asset_code", &asset_code, ALLOWED_ASSET_CODES)?;

    Ok(())
}

pub fn validate_positive_amount(amount: &BigDecimal) -> ValidationResult {
    if amount <= &BigDecimal::from(0) {
        return Err(ValidationError::new("amount", "must be greater than zero"));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::str::FromStr;

    fn valid_stellar_address() -> String {
        "G".to_owned() + &"A".repeat(55)
    }

    #[test]
    fn validates_required_field() {
        assert!(validate_required("field", "value").is_ok());
        assert!(validate_required("field", "   ").is_err());
    }

    #[test]
    fn validates_max_len() {
        assert!(validate_max_len("field", "abc", 3).is_ok());
        assert!(validate_max_len("field", "abcd", 3).is_err());
    }

    #[test]
    fn validates_enum_values() {
        assert!(validate_enum("status", "pending", &["pending", "completed"]).is_ok());
        assert!(validate_enum("status", "unknown", &["pending", "completed"]).is_err());
    }

    #[test]
    fn sanitizes_string() {
        assert_eq!(sanitize_string("  hello\tworld  "), "hello world");
        assert_eq!(sanitize_string("single"), "single");
        assert_eq!(sanitize_string(" \n "), "");
        assert_eq!(sanitize_string("ab\u{0000}cd\u{0007}"), "abcd");
    }

    #[test]
    fn validates_stellar_address() {
        assert!(validate_stellar_address(&valid_stellar_address()).is_ok());
        assert!(validate_stellar_address("GSHORT").is_err());
        assert!(validate_stellar_address(&("g".to_owned() + &"A".repeat(55))).is_err());
        assert!(validate_stellar_address(&("G".to_owned() + &"a".repeat(55))).is_err());
        assert!(validate_stellar_address(&format!(" {} ", valid_stellar_address())).is_ok());
    }

    #[test]
    fn validates_asset_code() {
        assert!(validate_asset_code("USD").is_ok());
        assert!(validate_asset_code("  USD  ").is_ok());
        assert!(validate_asset_code("usd").is_err());
        assert!(validate_asset_code("EUR").is_err());
        assert!(validate_asset_code(&"A".repeat(13)).is_err());
        assert!(validate_asset_code("US D").is_err());
        assert!(validate_asset_code("").is_err());
    }

    #[test]
    fn validates_positive_amount() {
        let positive = BigDecimal::from_str("1.23").expect("valid decimal");
        let zero = BigDecimal::from(0);
        let negative = BigDecimal::from(-1);

        assert!(validate_positive_amount(&positive).is_ok());
        assert!(validate_positive_amount(&zero).is_err());
        assert!(validate_positive_amount(&negative).is_err());
    }

    #[test]
    fn strict_payload_accepts_known_fields() {
        #[derive(Debug, Deserialize, PartialEq, Eq)]
        struct Payload {
            id: String,
            status: String,
        }

        let parsed: StrictPayload<Payload> =
            serde_json::from_str(r#"{"id":"tx-1","status":"pending"}"#).expect("valid payload");

        assert_eq!(
            parsed.data,
            Payload {
                id: "tx-1".to_string(),
                status: "pending".to_string()
            }
        );
    }

    #[test]
    fn strict_payload_rejects_unknown_fields() {
        #[derive(Debug, Deserialize)]
        struct Payload {
            id: String,
        }

        let parsed = serde_json::from_str::<StrictPayload<Payload>>(r#"{"id":"tx-1","extra":"x"}"#);
        assert!(parsed.is_err());
    }
}
