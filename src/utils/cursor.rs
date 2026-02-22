use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Cursor helpers: encode/decode a (created_at, id) tuple into a base64 string.
/// Format used internally: "{created_at_rfc3339}|{uuid}" then base64 encoded.

use base64::{Engine as _, engine::general_purpose};

pub fn encode(created_at: DateTime<Utc>, id: Uuid) -> String {
    let s = format!("{}|{}", created_at.to_rfc3339(), id.to_string());
    general_purpose::STANDARD.encode(s)
}

pub fn decode(cursor: &str) -> Result<(DateTime<Utc>, Uuid), String> {
    let decoded = general_purpose::STANDARD.decode(cursor).map_err(|e| format!("base64 decode error: {}", e))?;
    let s = String::from_utf8(decoded).map_err(|e| format!("utf8 error: {}", e))?;
    let mut parts = s.splitn(2, '|');
    let ts_str = parts.next().ok_or_else(|| "missing timestamp in cursor".to_string())?;
    let id_str = parts.next().ok_or_else(|| "missing id in cursor".to_string())?;
    let ts = DateTime::parse_from_rfc3339(ts_str)
        .map_err(|e| format!("timestamp parse error: {}", e))?
        .with_timezone(&Utc);
    let id = Uuid::parse_str(id_str).map_err(|e| format!("uuid parse error: {}", e))?;
    Ok((ts, id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn test_encode_decode_roundtrip() {
        let timestamp = Utc.with_ymd_and_hms(2023, 12, 25, 10, 30, 45).unwrap();
        let id = Uuid::new_v4();

        let encoded = encode(timestamp, id);
        let (decoded_ts, decoded_id) = decode(&encoded).unwrap();

        assert_eq!(timestamp, decoded_ts);
        assert_eq!(id, decoded_id);
    }

    #[test]
    fn test_encode_produces_base64() {
        let timestamp = Utc::now();
        let id = Uuid::new_v4();

        let encoded = encode(timestamp, id);
        
        // Should be valid base64
        assert!(general_purpose::STANDARD.decode(&encoded).is_ok());
    }

    #[test]
    fn test_decode_invalid_base64() {
        let result = decode("invalid_base64!");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("base64 decode error"));
    }

    #[test]
    fn test_decode_invalid_utf8() {
        // Create invalid UTF-8 by encoding some bytes that aren't valid UTF-8
        let invalid_utf8 = general_purpose::STANDARD.encode(&[0xFF, 0xFE, 0xFD]);
        let result = decode(&invalid_utf8);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("utf8 error"));
    }

    #[test]
    fn test_decode_missing_separator() {
        let no_separator = general_purpose::STANDARD.encode("2023-12-25T10:30:45Z");
        let result = decode(&no_separator);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing id in cursor"));
    }

    #[test]
    fn test_decode_invalid_timestamp() {
        let invalid_ts = general_purpose::STANDARD.encode("invalid-timestamp|550e8400-e29b-41d4-a716-446655440000");
        let result = decode(&invalid_ts);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("timestamp parse error"));
    }

    #[test]
    fn test_decode_invalid_uuid() {
        let invalid_uuid = general_purpose::STANDARD.encode("2023-12-25T10:30:45Z|invalid-uuid");
        let result = decode(&invalid_uuid);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("uuid parse error"));
    }

    #[test]
    fn test_encode_decode_with_specific_values() {
        let timestamp = Utc.with_ymd_and_hms(2023, 1, 1, 0, 0, 0).unwrap();
        let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();

        let encoded = encode(timestamp, id);
        let (decoded_ts, decoded_id) = decode(&encoded).unwrap();

        assert_eq!(timestamp, decoded_ts);
        assert_eq!(id, decoded_id);
    }
}