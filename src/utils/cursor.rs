use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Cursor helpers: encode/decode a (created_at, id) tuple into a base64 string.
/// Format used internally: "{created_at_rfc3339}|{uuid}" then base64 encoded.

pub fn encode(created_at: DateTime<Utc>, id: Uuid) -> String {
    let s = format!("{}|{}", created_at.to_rfc3339(), id.to_string());
    base64::encode(s)
}

pub fn decode(cursor: &str) -> Result<(DateTime<Utc>, Uuid), String> {
    let decoded = base64::decode(cursor).map_err(|e| format!("base64 decode error: {}", e))?;
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
