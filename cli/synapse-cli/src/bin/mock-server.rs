//! Mock HTTP server binary used by CLI integration tests.
//!
//! Binds to the address given in `MOCK_SERVER_ADDR` (default `127.0.0.1:4010`).
//! The `MOCK_SERVER_SCENARIO` env-var selects the response scenario:
//!
//! | Scenario  | Description                                              |
//! |-----------|----------------------------------------------------------|
//! | `happy`   | (default) Normal responses with canned non-empty data.  |
//! | `edge`    | Edge-case responses (empty lists, session_expired, …).  |
//!
//! Routes served:
//!   POST /admin/reconciliation/run
//!   GET  /admin/reconciliation/reports?…
//!   GET  /admin/reconciliation/reports/<id>
//!   GET  /events
//!   POST /reconnect
//!   GET  /reconnect/status
//!   GET  /reconnect/status?token=…

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};

const DEFAULT_ADDRESS: &str = "127.0.0.1:4010";
const SAMPLE_REPORT_ID: &str = "3f1d8c31-5f1d-4fb8-93e0-112233445566";
const SAMPLE_SESSION_ID: &str = "550e8400-e29b-41d4-a716-446655440000";

fn main() -> std::io::Result<()> {
    let address = std::env::var("MOCK_SERVER_ADDR")
        .unwrap_or_else(|_| DEFAULT_ADDRESS.to_string());
    let scenario = std::env::var("MOCK_SERVER_SCENARIO")
        .unwrap_or_else(|_| "happy".to_string());

    let listener = TcpListener::bind(&address)?;
    eprintln!("Mock Synapse API listening on http://{address} (scenario={scenario})");

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(err) = handle_connection(stream, &scenario) {
                    eprintln!("mock server error: {err}");
                }
            }
            Err(err) => eprintln!("mock server accept error: {err}"),
        }
    }

    Ok(())
}

fn handle_connection(stream: TcpStream, scenario: &str) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);

    // Read the request line (e.g. "POST /reconnect HTTP/1.1").
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    if request_line.is_empty() {
        return Ok(());
    }

    // Drain headers and collect Content-Length so we can read the body.
    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break; // end of headers
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }

    // Read body (needed so POST requests are fully consumed).
    let mut body_bytes = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body_bytes)?;
    }
    let _body = String::from_utf8_lossy(&body_bytes);

    let response = route(request_line.trim_end(), scenario);
    let mut stream = stream;
    stream.write_all(response.as_bytes())?;
    stream.flush()?;
    Ok(())
}

fn route(request_line: &str, scenario: &str) -> String {
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();

    match (method, path) {
        // ── Reconciliation ────────────────────────────────────────────────────
        ("POST", "/admin/reconciliation/run") => {
            if scenario == "edge" {
                json_response(200, r#"{
  "message": "Reconciliation completed successfully",
  "report": {
    "id": "3f1d8c31-5f1d-4fb8-93e0-112233445566",
    "generated_at": "2026-06-27T06:10:12Z",
    "period_start": "2026-06-26T06:10:12Z",
    "period_end": "2026-06-27T06:10:12Z",
    "total_db_transactions": 0,
    "total_chain_payments": 0,
    "missing_on_chain_count": 0,
    "orphaned_payments_count": 0,
    "amount_mismatches_count": 0,
    "has_discrepancies": false
  }
}"#)
            } else {
                json_response(200, r#"{
  "message": "Reconciliation completed successfully",
  "report": {
    "id": "3f1d8c31-5f1d-4fb8-93e0-112233445566",
    "generated_at": "2026-06-27T06:10:12Z",
    "period_start": "2026-06-26T06:10:12Z",
    "period_end": "2026-06-27T06:10:12Z",
    "total_db_transactions": 12,
    "total_chain_payments": 11,
    "missing_on_chain_count": 1,
    "orphaned_payments_count": 0,
    "amount_mismatches_count": 1,
    "has_discrepancies": true
  }
}"#)
            }
        }

        ("GET", p) if p.starts_with("/admin/reconciliation/reports?") => {
            let query = p.split_once('?').map(|(_, q)| q).unwrap_or_default();
            let params = parse_query(query);
            let limit = params.get("limit").and_then(|v| v.parse::<i32>().ok()).unwrap_or(20);
            let offset = params.get("offset").and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);

            let body = if scenario == "edge" {
                format!(
                    r#"{{
  "reports": [],
  "total": 0,
  "limit": {limit},
  "offset": {offset}
}}"#
                )
            } else {
                format!(
                    r#"{{
  "reports": [
    {{
      "id": "{SAMPLE_REPORT_ID}",
      "generated_at": "2026-06-27T06:10:12Z",
      "period_start": "2026-06-26T06:10:12Z",
      "period_end": "2026-06-27T06:10:12Z",
      "total_db_transactions": 12,
      "total_chain_payments": 11,
      "missing_on_chain_count": 1,
      "orphaned_payments_count": 0,
      "amount_mismatches_count": 1,
      "has_discrepancies": true
    }}
  ],
  "total": 1,
  "limit": {limit},
  "offset": {offset}
}}"#
                )
            };
            json_response(200, &body)
        }

        ("GET", p) if p.starts_with("/admin/reconciliation/reports/") => {
            let report_id = p.rsplit('/').next().unwrap_or(SAMPLE_REPORT_ID);
            let body = if scenario == "edge" {
                format!(
                    r#"{{
  "id": "{report_id}",
  "generated_at": "2026-06-27T06:10:12Z",
  "period_start": "2026-06-26T06:10:12Z",
  "period_end": "2026-06-27T06:10:12Z",
  "summary": {{
    "total_db_transactions": 0,
    "total_chain_payments": 0,
    "missing_on_chain_count": 0,
    "orphaned_payments_count": 0,
    "amount_mismatches_count": 0,
    "has_discrepancies": false
  }},
  "missing_on_chain": [],
  "orphaned_payments": [],
  "amount_mismatches": []
}}"#
                )
            } else {
                format!(
                    r#"{{
  "id": "{report_id}",
  "generated_at": "2026-06-27T06:10:12Z",
  "period_start": "2026-06-26T06:10:12Z",
  "period_end": "2026-06-27T06:10:12Z",
  "summary": {{
    "total_db_transactions": 12,
    "total_chain_payments": 11,
    "missing_on_chain_count": 1,
    "orphaned_payments_count": 0,
    "amount_mismatches_count": 1,
    "has_discrepancies": true
  }},
  "missing_on_chain": [],
  "orphaned_payments": [],
  "amount_mismatches": []
}}"#
                )
            };
            json_response(200, &body)
        }

        // ── Events watch ──────────────────────────────────────────────────────
        ("GET", "/events") => json_response(
            200,
            r#"[
  {
    "transaction_id": "aaaaaaaa-0000-0000-0000-000000000001",
    "status": "pending",
    "timestamp": "2026-06-27T06:00:00Z",
    "message": null
  },
  {
    "transaction_id": "aaaaaaaa-0000-0000-0000-000000000002",
    "status": "completed",
    "timestamp": "2026-06-27T06:05:00Z",
    "message": "settled"
  }
]"#,
        ),

        // ── POST /reconnect ───────────────────────────────────────────────────
        //
        // Happy path: returns `type: reconnect`, `status: ready`.
        // Edge scenario: returns `type: reconnect`, `status: session_expired`.
        ("POST", "/reconnect") => {
            if scenario == "edge" {
                json_response(200, r#"{
  "type": "reconnect",
  "status": {"status": "session_expired"},
  "backoff_seconds": 0,
  "requires_resync": false
}"#)
            } else {
                json_response(200, &format!(r#"{{
  "type": "reconnect",
  "status": {{"status": "ready", "session_id": "{SAMPLE_SESSION_ID}"}},
  "backoff_seconds": 5,
  "requires_resync": false
}}"#))
            }
        }

        // ── GET /reconnect/status  (no query string — fresh session) ──────────
        //
        // Edge scenario: the server still returns `ready` for a new session;
        // "no cursor" is not an error.
        ("GET", "/reconnect/status") => {
            // Always ready — the "no cursor" edge case must never error.
            json_response(200, &format!(r#"{{
  "type": "reconnect",
  "status": {{"status": "ready", "session_id": "{SAMPLE_SESSION_ID}"}},
  "backoff_seconds": 1,
  "requires_resync": true
}}"#))
        }

        // ── GET /reconnect/status?token=… ─────────────────────────────────────
        //
        // Happy path: known token → ready.
        // Edge scenario: token present → session_expired.
        ("GET", p) if p.starts_with("/reconnect/status?") => {
            if scenario == "edge" {
                json_response(200, r#"{
  "type": "reconnect",
  "status": {"status": "session_expired"},
  "backoff_seconds": 0,
  "requires_resync": false
}"#)
            } else {
                json_response(200, &format!(r#"{{
  "type": "reconnect",
  "status": {{"status": "ready", "session_id": "{SAMPLE_SESSION_ID}"}},
  "backoff_seconds": 5,
  "requires_resync": false
}}"#))
            }
        }

        // ── Catch-all ─────────────────────────────────────────────────────────
        _ => json_response(404, r#"{"error": "Not found"}"#),
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn json_response(status: u16, body: &str) -> String {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "OK",
    };
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn parse_query(query: &str) -> std::collections::HashMap<String, String> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}
