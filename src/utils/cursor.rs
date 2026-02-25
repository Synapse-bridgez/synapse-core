use base64::{engine::general_purpose, Engine as _};
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Cursor helpers: encode/decode a (created_at, id) tuple into a base64 string.
/// Format used internally: "{created_at_rfc3339}|{uuid}" then base64 encoded.
pub fn encode(created_at: DateTime<Utc>, id: Uuid) -> String {
    let s = format!("{}|{}", created_at.to_rfc3339(), id);
    general_purpose::STANDARD.encode(s)
}

pub fn decode(cursor: &str) -> Result<(DateTime<Utc>, Uuid), String> {
    let decoded = general_purpose::STANDARD
        .decode(cursor)
        .map_err(|e| format!("base64 decode error: {}", e))?;
    let s = String::from_utf8(decoded).map_err(|e| format!("utf8 error: {}", e))?;
    let mut parts = s.splitn(2, '|');
    let ts_str = parts
        .next()
        .ok_or_else(|| "missing timestamp in cursor".to_string())?;
    let id_str = parts
        .next()
        .ok_or_else(|| "missing id in cursor".to_string())?;
    let ts = DateTime::parse_from_rfc3339(ts_str)
        .map_err(|e| format!("timestamp parse error: {}", e))?
        .with_timezone(&Utc);
    let id = Uuid::parse_str(id_str).map_err(|e| format!("uuid parse error: {}", e))?;
    Ok((ts, id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    #[test]
    fn test_cursor_encode_decode_roundtrip() {
        let created_at = Utc::now();
        let id = Uuid::new_v4();
        let cursor = encode(created_at, id);
        let (decoded_ts, decoded_id) = decode(&cursor).unwrap();
        assert_eq!(created_at, decoded_ts);
        assert_eq!(id, decoded_id);
    }

    #[test]
    fn test_cursor_decode_invalid_base64() {
        let result = decode("invalid_base64!");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("base64 decode error"));
    }

    #[test]
    fn test_cursor_decode_malformed_data() {
        // Base64 of "no_separator" -> "bm9fc2VwYXJhdG9y"
        let cursor = "bm9fc2VwYXJhdG9y";
        let result = decode(cursor);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing id in cursor"));
    }

    #[test]
    fn test_cursor_decode_invalid_uuid() {
        // Valid timestamp, invalid UUID
        let data = "2023-01-01T00:00:00+00:00|invalid-uuid";
        let cursor = base64::encode(data);
        let result = decode(&cursor);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("uuid parse error"));
    }

    #[test]
    fn test_cursor_decode_invalid_timestamp() {
        // Invalid timestamp, valid UUID
        let data = "invalid-timestamp|12345678-1234-1234-1234-123456789012";
        let cursor = base64::encode(data);
        let result = decode(&cursor);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("timestamp parse error"));
    }
}
