//! Cursor-based pagination for WebSocket real-time event streams.
//!
//! # Design
//!
//! Offset-based pagination is unsuitable for live event streams because new
//! events shift offsets while a client is paginating, causing skips or
//! duplicates. This module implements **cursor-based pagination**: every page
//! carries an opaque `next_cursor` that encodes the position of the last item
//! delivered. The client sends that cursor on the next request to receive
//! exactly the following page — no duplicates, no gaps.
//!
//! ## Cursor encoding
//!
//! A cursor is a base64url-encoded JSON object:
//! ```json
//! { "last_id": "<event-id>", "ts": "<rfc3339-timestamp>" }
//! ```
//! The timestamp is included so the server can fall back to a time-based
//! index scan when the event ID is no longer in the hot window.  Cursors are
//! treated as opaque by clients; internal structure is not part of the API
//! contract.
//!
//! ## Security
//!
//! - `page_size` is clamped to `[1, MAX_PAGE_SIZE]`; oversized requests are
//!   rejected with [`PaginationError::InvalidPageSize`].
//! - Cursor strings are length-limited and validated before decoding to
//!   prevent large-payload or injection attacks.
//! - No raw SQL is produced here; callers receive typed [`PaginationParams`]
//!   and use parameterised queries.
//!
//! ## Performance
//!
//! Decoded cursors carry both an ID and a timestamp so an index on
//! `(created_at, id)` can satisfy the query with a single range scan.
//! Page sizes are bounded to avoid unbounded serialisation work per frame.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Hard upper bound on events per page.  Prevents accidental or malicious
/// oversized frames from saturating the send buffer.
pub const MAX_PAGE_SIZE: u32 = 100;

/// Default number of events per page when the client omits `page_size`.
pub const DEFAULT_PAGE_SIZE: u32 = 20;

/// Maximum byte length accepted for a raw cursor string.
/// base64url(64 bytes) ≈ 88 chars; 512 gives comfortable headroom.
const MAX_CURSOR_BYTES: usize = 512;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors produced by the pagination layer.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PaginationError {
    /// `page_size` was zero or exceeded [`MAX_PAGE_SIZE`].
    #[error("page_size must be between 1 and {MAX_PAGE_SIZE}, got {0}")]
    InvalidPageSize(u32),

    /// The cursor string was too long to be a valid cursor.
    #[error("cursor exceeds maximum length of {MAX_CURSOR_BYTES} bytes")]
    CursorTooLong,

    /// The cursor string contained characters outside the base64url alphabet.
    #[error("cursor contains invalid characters")]
    CursorInvalidEncoding,

    /// The cursor decoded successfully but its JSON payload was malformed.
    #[error("cursor payload is malformed: {0}")]
    CursorMalformed(String),
}

// ---------------------------------------------------------------------------
// Cursor
// ---------------------------------------------------------------------------

/// Internal representation of a decoded pagination cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    /// ID of the last event delivered on the previous page.
    pub last_id: String,
    /// Creation timestamp of that event, used for index-assisted range scans.
    pub ts: DateTime<Utc>,
}

impl Cursor {
    /// Encodes the cursor to an opaque base64url string safe for transmission
    /// over WebSocket frames and query parameters.
    pub fn encode(&self) -> String {
        let json = serde_json::to_string(self).expect("cursor serialisation is infallible");
        base64_encode(json.as_bytes())
    }

    /// Decodes and validates a raw cursor string received from a client.
    ///
    /// # Errors
    ///
    /// Returns [`PaginationError::CursorTooLong`] if the string exceeds
    /// [`MAX_CURSOR_BYTES`], [`PaginationError::CursorInvalidEncoding`] if
    /// it is not valid base64url, or [`PaginationError::CursorMalformed`]
    /// if the decoded payload is not a valid cursor JSON object.
    pub fn decode(raw: &str) -> Result<Self, PaginationError> {
        if raw.len() > MAX_CURSOR_BYTES {
            return Err(PaginationError::CursorTooLong);
        }
        let bytes = base64_decode(raw)?;
        serde_json::from_slice::<Cursor>(&bytes)
            .map_err(|e| PaginationError::CursorMalformed(e.to_string()))
    }
}

impl fmt::Display for Cursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.encode())
    }
}

// ---------------------------------------------------------------------------
// Pagination request / response
// ---------------------------------------------------------------------------

/// Parameters extracted from a client's pagination request.
///
/// Construct via [`PaginationRequest::parse`] which performs all validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaginationParams {
    /// Validated and clamped number of events to return.
    pub page_size: u32,
    /// Decoded cursor, or `None` for the first page.
    pub cursor: Option<Cursor>,
}

/// Raw client-supplied pagination fields, typically deserialised from the
/// WebSocket message JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaginationRequest {
    /// Requested page size.  Clamped to `[1, MAX_PAGE_SIZE]`.
    #[serde(default = "default_page_size")]
    pub page_size: u32,

    /// Opaque cursor string from the previous response, or absent for the
    /// first page.
    pub cursor: Option<String>,
}

fn default_page_size() -> u32 {
    DEFAULT_PAGE_SIZE
}

impl PaginationRequest {
    /// Validates the request and returns typed [`PaginationParams`].
    ///
    /// # Errors
    ///
    /// - [`PaginationError::InvalidPageSize`] when `page_size` is 0 or > [`MAX_PAGE_SIZE`].
    /// - Cursor errors forwarded from [`Cursor::decode`].
    pub fn parse(self) -> Result<PaginationParams, PaginationError> {
        if self.page_size == 0 || self.page_size > MAX_PAGE_SIZE {
            return Err(PaginationError::InvalidPageSize(self.page_size));
        }
        let cursor = self
            .cursor
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(Cursor::decode)
            .transpose()?;
        Ok(PaginationParams {
            page_size: self.page_size,
            cursor,
        })
    }
}

/// A page of events returned to the WebSocket client.
///
/// `E` is the application-level event type. The caller is responsible for
/// serialising this to the wire format.
#[derive(Debug, Serialize, Deserialize)]
pub struct EventPage<E> {
    /// The events on this page, at most `page_size` entries.
    pub events: Vec<E>,

    /// Opaque cursor for the next page, or `None` when there are no more
    /// events in the current window.
    pub next_cursor: Option<String>,

    /// Whether more events may be available after this page.
    pub has_more: bool,

    /// Total events requested (i.e. the effective `page_size`).
    pub page_size: u32,
}

impl<E> EventPage<E> {
    /// Constructs an `EventPage` from a raw event slice.
    ///
    /// `events` should contain **at most** `page_size + 1` items: the extra
    /// item is used as a look-ahead to determine `has_more` without a
    /// separate COUNT query.  It is stripped before returning.
    ///
    /// `cursor_fn` maps the last delivered event to its cursor.  Callers
    /// typically implement this by reading the event ID and timestamp.
    pub fn from_lookahead<F>(mut events: Vec<E>, page_size: u32, cursor_fn: F) -> Self
    where
        F: Fn(&E) -> Cursor,
    {
        let has_more = events.len() > page_size as usize;
        if has_more {
            events.truncate(page_size as usize);
        }
        let next_cursor = if has_more {
            events.last().map(|e| cursor_fn(e).encode())
        } else {
            None
        };
        Self {
            has_more,
            next_cursor,
            page_size,
            events,
        }
    }
}

// ---------------------------------------------------------------------------
// base64url helpers (no padding, URL-safe alphabet)
// ---------------------------------------------------------------------------

fn base64_encode(input: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(input)
}

fn base64_decode(input: &str) -> Result<Vec<u8>, PaginationError> {
    // Validate alphabet before decoding to give a clear error.
    if !input
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(PaginationError::CursorInvalidEncoding);
    }
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(input)
        .map_err(|_| PaginationError::CursorInvalidEncoding)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn fixed_ts() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2024, 6, 1, 12, 0, 0).unwrap()
    }

    fn make_cursor(id: &str) -> Cursor {
        Cursor {
            last_id: id.to_string(),
            ts: fixed_ts(),
        }
    }

    // --- Cursor encode / decode ---

    #[test]
    fn test_cursor_encode_decode_roundtrip() {
        let c = make_cursor("evt-abc-123");
        let encoded = c.encode();
        let decoded = Cursor::decode(&encoded).unwrap();
        assert_eq!(decoded.last_id, "evt-abc-123");
        assert_eq!(decoded.ts, fixed_ts());
    }

    #[test]
    fn test_cursor_decode_too_long() {
        let long = "a".repeat(MAX_CURSOR_BYTES + 1);
        assert_eq!(Cursor::decode(&long), Err(PaginationError::CursorTooLong));
    }

    #[test]
    fn test_cursor_decode_invalid_characters() {
        // '+' and '/' are standard base64 but not base64url
        assert_eq!(
            Cursor::decode("abc+def/"),
            Err(PaginationError::CursorInvalidEncoding)
        );
    }

    #[test]
    fn test_cursor_decode_valid_base64url_but_bad_json() {
        // base64url-encode some garbage bytes that are not valid JSON
        let garbage = base64_encode(b"not-json!!!");
        assert!(matches!(
            Cursor::decode(&garbage),
            Err(PaginationError::CursorMalformed(_))
        ));
    }

    #[test]
    fn test_cursor_display_equals_encode() {
        let c = make_cursor("evt-1");
        assert_eq!(format!("{c}"), c.encode());
    }

    // --- PaginationRequest::parse ---

    #[test]
    fn test_parse_defaults() {
        let req = PaginationRequest {
            page_size: DEFAULT_PAGE_SIZE,
            cursor: None,
        };
        let params = req.parse().unwrap();
        assert_eq!(params.page_size, DEFAULT_PAGE_SIZE);
        assert!(params.cursor.is_none());
    }

    #[test]
    fn test_parse_page_size_zero_is_error() {
        let req = PaginationRequest {
            page_size: 0,
            cursor: None,
        };
        assert_eq!(req.parse(), Err(PaginationError::InvalidPageSize(0)));
    }

    #[test]
    fn test_parse_page_size_too_large_is_error() {
        let req = PaginationRequest {
            page_size: MAX_PAGE_SIZE + 1,
            cursor: None,
        };
        assert_eq!(
            req.parse(),
            Err(PaginationError::InvalidPageSize(MAX_PAGE_SIZE + 1))
        );
    }

    #[test]
    fn test_parse_max_page_size_accepted() {
        let req = PaginationRequest {
            page_size: MAX_PAGE_SIZE,
            cursor: None,
        };
        assert!(req.parse().is_ok());
    }

    #[test]
    fn test_parse_page_size_one_accepted() {
        let req = PaginationRequest {
            page_size: 1,
            cursor: None,
        };
        let params = req.parse().unwrap();
        assert_eq!(params.page_size, 1);
    }

    #[test]
    fn test_parse_with_valid_cursor() {
        let cursor_str = make_cursor("evt-999").encode();
        let req = PaginationRequest {
            page_size: 10,
            cursor: Some(cursor_str),
        };
        let params = req.parse().unwrap();
        assert_eq!(params.cursor.unwrap().last_id, "evt-999");
    }

    #[test]
    fn test_parse_with_empty_cursor_string_treated_as_none() {
        let req = PaginationRequest {
            page_size: 10,
            cursor: Some(String::new()),
        };
        let params = req.parse().unwrap();
        assert!(params.cursor.is_none());
    }

    #[test]
    fn test_parse_with_invalid_cursor_propagates_error() {
        let req = PaginationRequest {
            page_size: 10,
            cursor: Some("not+valid/base64url".to_string()),
        };
        assert_eq!(req.parse(), Err(PaginationError::CursorInvalidEncoding));
    }

    // --- EventPage ---

    #[derive(Debug, PartialEq)]
    struct Event {
        id: String,
        ts: DateTime<Utc>,
        payload: u32,
    }

    fn cursor_for(e: &Event) -> Cursor {
        Cursor {
            last_id: e.id.clone(),
            ts: e.ts,
        }
    }

    fn make_events(n: usize) -> Vec<Event> {
        (0..n)
            .map(|i| Event {
                id: format!("evt-{i}"),
                ts: fixed_ts() + chrono::Duration::seconds(i as i64),
                payload: i as u32,
            })
            .collect()
    }

    #[test]
    fn test_event_page_first_page_no_more() {
        // Exactly page_size events, no look-ahead extra → no next cursor.
        let events = make_events(5);
        let page = EventPage::from_lookahead(events, 5, cursor_for);
        assert_eq!(page.events.len(), 5);
        assert!(!page.has_more);
        assert!(page.next_cursor.is_none());
    }

    #[test]
    fn test_event_page_has_more_and_cursor() {
        // page_size=3, but we pass 4 events (look-ahead).
        let events = make_events(4);
        let page = EventPage::from_lookahead(events, 3, cursor_for);
        assert_eq!(page.events.len(), 3);
        assert!(page.has_more);
        assert!(page.next_cursor.is_some());

        // Cursor must decode to the last delivered event.
        let cursor_str = page.next_cursor.unwrap();
        let decoded = Cursor::decode(&cursor_str).unwrap();
        assert_eq!(decoded.last_id, "evt-2"); // 0-indexed, third item
    }

    #[test]
    fn test_event_page_empty_events() {
        let page: EventPage<Event> = EventPage::from_lookahead(vec![], 10, cursor_for);
        assert!(page.events.is_empty());
        assert!(!page.has_more);
        assert!(page.next_cursor.is_none());
    }

    #[test]
    fn test_event_page_single_event_no_more() {
        let events = make_events(1);
        let page = EventPage::from_lookahead(events, 10, cursor_for);
        assert_eq!(page.events.len(), 1);
        assert!(!page.has_more);
        assert!(page.next_cursor.is_none());
    }

    #[test]
    fn test_event_page_exact_lookahead_boundary() {
        // page_size items with no extra → has_more = false.
        let events = make_events(10);
        let page = EventPage::from_lookahead(events, 10, cursor_for);
        assert_eq!(page.events.len(), 10);
        assert!(!page.has_more);
    }

    #[test]
    fn test_event_page_page_size_stored() {
        let events = make_events(3);
        let page = EventPage::from_lookahead(events, 20, cursor_for);
        assert_eq!(page.page_size, 20);
    }

    // --- Serialisation ---

    #[test]
    fn test_pagination_request_deserialise_with_defaults() {
        // When page_size is absent the serde default fires.
        let json = r#"{}"#;
        let req: PaginationRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.page_size, DEFAULT_PAGE_SIZE);
        assert!(req.cursor.is_none());
    }

    #[test]
    fn test_pagination_request_deserialise_with_values() {
        let cursor_str = make_cursor("evt-42").encode();
        let json = format!(r#"{{"page_size":50,"cursor":"{cursor_str}"}}"#);
        let req: PaginationRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req.page_size, 50);
        assert_eq!(req.cursor.as_deref(), Some(cursor_str.as_str()));
    }

    #[test]
    fn test_event_page_serialises_to_json() {
        let events = make_events(2);
        // Wrap in a simple serialisable type.
        #[derive(Serialize, Deserialize, Debug)]
        struct SimpleEvent {
            id: String,
        }
        let simple: Vec<SimpleEvent> = events
            .iter()
            .map(|e| SimpleEvent { id: e.id.clone() })
            .collect();
        let page: EventPage<SimpleEvent> = EventPage {
            events: simple,
            next_cursor: Some("cursor123".to_string()),
            has_more: true,
            page_size: 2,
        };
        let json = serde_json::to_string(&page).unwrap();
        assert!(json.contains("has_more"));
        assert!(json.contains("next_cursor"));
        assert!(json.contains("cursor123"));
    }

    // --- Security edge cases ---

    #[test]
    fn test_cursor_decode_at_exact_length_limit() {
        // A cursor exactly MAX_CURSOR_BYTES long (mostly padding) should be
        // attempted: it either decodes or returns CursorMalformed, not CursorTooLong.
        let at_limit = "a".repeat(MAX_CURSOR_BYTES);
        let result = Cursor::decode(&at_limit);
        assert_ne!(result, Err(PaginationError::CursorTooLong));
    }

    #[test]
    fn test_cursor_decode_one_over_limit_is_too_long() {
        let over = "a".repeat(MAX_CURSOR_BYTES + 1);
        assert_eq!(Cursor::decode(&over), Err(PaginationError::CursorTooLong));
    }

    #[test]
    fn test_no_sql_injection_via_cursor_last_id() {
        // Demonstrate that the cursor last_id is treated as an opaque string,
        // not interpolated into queries. The value survives a roundtrip intact.
        let c = Cursor {
            last_id: "'; DROP TABLE events; --".to_string(),
            ts: fixed_ts(),
        };
        let decoded = Cursor::decode(&c.encode()).unwrap();
        assert_eq!(decoded.last_id, "'; DROP TABLE events; --");
        // Callers must bind this via parameterised query — never interpolate.
    }

    #[test]
    fn test_page_size_boundary_values() {
        for size in [1u32, MAX_PAGE_SIZE / 2, MAX_PAGE_SIZE] {
            let req = PaginationRequest {
                page_size: size,
                cursor: None,
            };
            assert!(req.parse().is_ok(), "page_size={size} should be valid");
        }
        for size in [0u32, MAX_PAGE_SIZE + 1, u32::MAX] {
            let req = PaginationRequest {
                page_size: size,
                cursor: None,
            };
            assert!(req.parse().is_err(), "page_size={size} should be rejected");
        }
    }
}
