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

const SAMPLE_REPORT_ID: &str = "3f1d8c31-5f1d-4fb8-93e0-112233445566";
const SAMPLE_LOCK_TOKEN: &str = "4e4e9e47-7e0f-4f2f-8d63-323c61279209";

fn main() -> std::io::Result<()> {
    let addr = std::env::var("MOCK_SERVER_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:4010".to_string());
    let scenario = std::env::var("MOCK_SERVER_SCENARIO")
        .unwrap_or_else(|_| "happy".to_string());

    let listener = TcpListener::bind(&addr)?;
    eprintln!("Mock Synapse API listening on http://{addr} (scenario={scenario})");
    let addr = std::env::var("MOCK_SERVER_ADDR").unwrap_or_else(|_| ADDRESS.to_string());
    let scenario = std::env::var("MOCK_SERVER_SCENARIO").unwrap_or_else(|_| "happy".to_string());
    let listener = TcpListener::bind(&addr)?;
    println!("Mock Synapse API listening on http://{addr}");
    let addr = std::env::var("MOCK_SERVER_ADDR").unwrap_or_else(|_| "127.0.0.1:4010".to_string());
    let scenario = std::env::var("MOCK_SERVER_SCENARIO").unwrap_or_else(|_| "happy".to_string());
    let listener = TcpListener::bind(addr)?;

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(err) = handle_connection(stream, &scenario) {
                    eprintln!("mock server error: {err}");
                if let Err(error) = handle_connection(stream, &scenario) {
                    eprintln!("mock server error: {error}");
                }
            }
            Err(error) => eprintln!("mock server accept error: {error}"),
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

    let scenario = std::env::var("MOCK_SERVER_SCENARIO")
        .unwrap_or_else(|_| "happy".to_string());

    let response = match scenario.as_str() {
        "edge" => route_edge(request_line.trim_end()),
        "not_found" => route_not_found(request_line.trim_end()),
        _ => route(request_line.trim_end()),
    };
    let response = route(request_line.trim_end(), scenario);
    let mut stream = stream;
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

/// Scenario: "not_found" — every transactions and settlements endpoint returns 404.
fn route_not_found(request_line: &str) -> String {
fn route(request_line: &str, scenario: &str) -> String {
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();
    let path_only = path.split('?').next().unwrap_or(path);

    match (method, path_only) {
        ("GET", p) if p.starts_with("/transactions/") => {
            json_response(404, r#"{"error":"transaction not found"}"#)
        }
        ("GET", p) if p.starts_with("/settlements/") => {
            json_response(404, r#"{"error":"settlement not found"}"#)
        }
        ("GET", "/settlements") => {
            json_response(404, r#"{"error":"not found"}"#)
        }
        // Fall back to happy-path for all other routes (health, stats, etc.)
        _ => route(request_line),
    }
}

/// Scenario: "edge" — empty/minimal payloads for reconciliation; everything
/// else falls through to the happy-path handler.
fn route_edge(request_line: &str) -> String {
    let mut parts = request_line.split_whitespace();
    let _method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();
    let path_only = path.split('?').next().unwrap_or(path);

    match path_only {
        // Return empty report list for the "edge" scenario
        p if p.starts_with("/admin/reconciliation/reports") && !p[1..].contains('/') => {
            json_response(
                200,
                r#"{"reports":[],"total":0,"limit":20,"offset":0}"#,
            )
        }
        // Individual report endpoint still returns a minimal no-discrepancy report
        p if p.starts_with("/admin/reconciliation/reports/") => {
            let report_id = path_only.rsplit('/').next()
                .unwrap_or("3f1d8c31-5f1d-4fb8-93e0-112233445566");
            json_response(
                200,
                &format!(
                    r#"{{
  "id": "{report_id}",
  "generated_at": "2026-06-30T06:15:00Z",
  "period_start": "2026-06-29T06:15:00Z",
  "period_end": "2026-06-30T06:15:00Z",
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
                ),
            )
        }
        // Run reconciliation: return a no-discrepancy result
        "/admin/reconciliation/run" => json_response(
            200,
            r#"{
  "message": "Reconciliation completed successfully",
  "report": {
    "id": "3f1d8c31-5f1d-4fb8-93e0-112233445566",
    "generated_at": "2026-06-30T06:15:00Z",
    "period_start": "2026-06-29T06:15:00Z",
    "period_end": "2026-06-30T06:15:00Z",
    match (method, path) {
        // ── Reconciliation ────────────────────────────────────────────────────
        ("POST", "/admin/reconciliation/run") => {
            let body = if scenario == "edge" {
                format!(
                    r#"{{
  "message": "Reconciliation completed successfully",
  "report": {{
    "id": "{SAMPLE_REPORT_ID}",
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
}"#,
        ),
        // All other routes: delegate to happy-path
        _ => route(request_line),
    }
}

fn route(request_line: &str) -> String {
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();

    // Strip query string for matching, keep it for param parsing
    let path_only = path.split('?').next().unwrap_or(path);

    match (method, path_only) {
        // ── Health ─────────────────────────────────────────────────────────────
        ("GET", "/live") => json_response(200, r#"{"status":"alive"}"#),

        ("GET", "/ready") => json_response(200, r#"{"status":"ready","draining":false}"#),

        ("GET", "/health") => json_response(
            200,
            r#"{
  "status": "healthy",
  "version": "0.1.0",
  "db": "connected",
  "db_pool": {
    "active_connections": 2,
    "idle_connections": 8,
    "max_connections": 10,
    "usage_percent": 20.0
  },
  "pending_queue_depth": 0,
  "current_batch_size": 50,
  "ws_connection_count": 3
}"#,
        ),

        ("GET", "/errors") => json_response(
            200,
            r#"{
  "errors": {
    "E001": "not found",
    "E002": "validation error",
    "E003": "internal server error"
  },
  "version": "1.0.0"
}"#,
        ),

        // ── Stats ──────────────────────────────────────────────────────────────
        ("GET", "/stats/status") => json_response(
            200,
            r#"[
  {"status":"pending","count":12},
  {"status":"completed","count":847},
  {"status":"failed","count":5},
  {"status":"cancelled","count":2}
]"#,
        ),

        ("GET", "/stats/daily") => {
            let query = path.split_once('?').map(|(_, q)| q).unwrap_or_default();
            let params = parse_query(query);
            let days = params
                .get("days")
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(7);

            // Build N days of sample data ending on 2026-06-30
            let mut rows = Vec::new();
            for i in (0..days).rev() {
                // Simple date arithmetic: subtract i days from 2026-06-30
                let (y, m, d) = subtract_days(2026, 6, 30, i);
                let count = 100 + (i * 7) % 50;
                let amount = 10000 + (i * 137) % 5000;
                rows.push(format!(
                    r#"  {{"date":"{y:04}-{m:02}-{d:02}","total_amount":"{amount}.00","transaction_count":{count}}}"#
                ));
            }
            json_response(200, &format!("[\n{}\n]", rows.join(",\n")))
        }

        ("GET", "/stats/assets") => json_response(
            200,
            r#"[
  {"asset_code":"USD","total_amount":"142350.00","transaction_count":521},
  {"asset_code":"EUR","total_amount":"87200.50","transaction_count":214},
  {"asset_code":"USDC","total_amount":"34100.00","transaction_count":143},
  {"asset_code":"XLM","total_amount":"9800.75","transaction_count":88}
]"#,
        ),

        ("GET", "/cache/metrics") => json_response(
            200,
            r#"{
  "query_cache": {"hits": 4820, "misses": 310, "size": 512},
  "idempotency_cache_hits": 1203,
  "idempotency_cache_misses": 47,
  "idempotency_lock_acquired": 980,
  "idempotency_lock_contention": 12,
  "idempotency_errors": 0,
  "idempotency_fallback_count": 3
}"#,
        ),

        // ── Admin / Reconciliation ─────────────────────────────────────────────
        ("POST", "/admin/reconciliation/run") => json_response(
            200,
            r#"{
  "message": "Reconciliation completed successfully",
  "report": {
    "id": "3f1d8c31-5f1d-4fb8-93e0-112233445566",
    "generated_at": "2026-06-30T06:15:00Z",
    "period_start": "2026-06-29T06:15:00Z",
    "period_end": "2026-06-30T06:15:00Z",
  }}
}}"#
                )
            } else {
                format!(
                    r#"{{
            if scenario == "edge" {
                json_response(200, &run_body(false, 0, 0))
            } else {
                r#"{
  "message": "Reconciliation completed successfully",
  "report": {{
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
  }
}"#,
        ),

        ("GET", path_ref) if path_ref.starts_with("/admin/reconciliation/reports") && !path_ref.contains('/') => {
            let query = path.split_once('?').map(|(_, q)| q).unwrap_or_default();
            let params = parse_query(query);
            let limit = params.get("limit").and_then(|v| v.parse::<u32>().ok()).unwrap_or(20);
            let offset = params.get("offset").and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
            json_response(
                200,
                &format!(
  }}
}}"#
                )
            };
            json_response(200, &body)
        }

        ("GET", path) if path.starts_with("/admin/reconciliation/reports?") => {
            let query = path.split_once('?').map(|(_, q)| q).unwrap_or_default();
            let params = parse_query(query);
            let limit = params
                .get("limit")
                .and_then(|v| v.parse::<i32>().ok())
                .unwrap_or(20);
            let offset = params
                .get("offset")
                .and_then(|v| v.parse::<i32>().ok())

            json_response(200, body)
                json_response(200, &run_body(true, 12, 11))
            }
        }
        ("GET", path) if path.starts_with("/admin/reconciliation/reports?") => {
            let query = path
                .split_once('?')
                .map(|(_, query)| query)
                .unwrap_or_default();
            let params = parse_query(query);
            let limit = params
                .get("limit")
                .and_then(|value| value.parse::<i32>().ok())
                .unwrap_or(20);
            let offset = params
                .get("offset")
                .and_then(|value| value.parse::<i32>().ok())
                .unwrap_or(0);

            if scenario == "edge" {
                json_response(
                    200,
                    &format!(r#"{{"reports":[],"total":0,"limit":{limit},"offset":{offset}}}"#),
                )
            } else {
                format!(
                    r#"{{
  "reports": [
    {{
      "id": "3f1d8c31-5f1d-4fb8-93e0-112233445566",
      "generated_at": "2026-06-30T06:15:00Z",
      "period_start": "2026-06-29T06:15:00Z",
      "period_end": "2026-06-30T06:15:00Z",
      "total_db_transactions": 12,
      "total_chain_payments": 11,
      "missing_on_chain_count": 1,
      "orphaned_payments_count": 0,
      "amount_mismatches_count": 1,
      "has_discrepancies": true
    }}
  ],
                json_response(
                    200,
                    &format!(
                        r#"{{
  "reports": [{}],
  "total": 1,
  "limit": {limit},
  "offset": {offset}
}}"#
                ),
            )
        }

        ("GET", path_ref) if path_ref.starts_with("/admin/reconciliation/reports/") => {
            let report_id = path_only.rsplit('/').next().unwrap_or("3f1d8c31-5f1d-4fb8-93e0-112233445566");
            json_response(
                200,
                &format!(
}}"#,
                        report_summary(true, 12, 11)
                    ),
                )
            };
            json_response(200, &body)
        }
        ("GET", path) if path.starts_with("/events/watch") => {
            let body = r#"[
  {
    "transaction_id": "550e8400-e29b-41d4-a716-446655440000",
    "status": "completed",
    "timestamp": "2024-01-15T10:30:00Z",
    "message": "Settlement finalized"
  },
  {
    "transaction_id": "550e8401-e29b-41d4-a716-446655440001",
    "status": "pending",
    "timestamp": "2024-01-15T10:31:00Z"
  }
]"#;
            json_response(200, body)
        }
        ("POST", "/admin/transactions/bulk-status") => {
            let body = if scenario == "edge" {
                r#"{
  "updated": 1,
  "failed": 1,
  "errors": [
    {
      "transaction_id": "550e8400-e29b-41d4-a716-446655440001",
      "error": "status transition not allowed"
    }
  ]
}"#
            } else {
                r#"{
  "updated": 2,
  "failed": 0,
  "errors": []
}"#
            };
            json_response(200, body)
            }
        }

        ("GET", path) if path.starts_with("/admin/reconciliation/reports/") => {
            let report_id = path.rsplit('/').next().unwrap_or(SAMPLE_REPORT_ID);
            if scenario == "edge" {
                json_response(200, &report_detail(report_id, false, 0, 0))
            } else {
                json_response(200, &report_detail(report_id, true, 12, 11))
            }
        }
        ("GET", "/admin/locks") => {
            if scenario == "edge" {
                json_response(200, r#"{"active_locks":[],"total":0,"overdue":0}"#)
            } else {
                json_response(200, &locks_body())
            }
        }
        _ => json_response(404, r#"{"error":"Not found"}"#),
    }
}

fn run_body(has_discrepancies: bool, db: i32, chain: i32) -> String {
    format!(
        r#"{{
  "message": "Reconciliation completed successfully",
  "report": {}
}}"#,
        report_summary(has_discrepancies, db, chain)
    )
}

fn report_summary(has_discrepancies: bool, db: i32, chain: i32) -> String {
    let missing = if has_discrepancies { 1 } else { 0 };
    let mismatch = if has_discrepancies { 1 } else { 0 };
    format!(
        r#"{{
  "id": "{SAMPLE_REPORT_ID}",
  "generated_at": "2026-06-27T06:10:12Z",
  "period_start": "2026-06-26T06:10:12Z",
  "period_end": "2026-06-27T06:10:12Z",
  "total_db_transactions": {db},
  "total_chain_payments": {chain},
  "missing_on_chain_count": {missing},
  "orphaned_payments_count": 0,
  "amount_mismatches_count": {mismatch},
  "has_discrepancies": {has_discrepancies}
}}"#
                )
            } else {
                format!(
                    r#"{{
    )
}

fn report_detail(report_id: &str, has_discrepancies: bool, db: i32, chain: i32) -> String {
    let missing = if has_discrepancies { 1 } else { 0 };
    let mismatch = if has_discrepancies { 1 } else { 0 };
    format!(
        r#"{{
  "id": "{report_id}",
  "generated_at": "2026-06-30T06:15:00Z",
  "period_start": "2026-06-29T06:15:00Z",
  "period_end": "2026-06-30T06:15:00Z",
  "summary": {{
    "total_db_transactions": {db},
    "total_chain_payments": {chain},
    "missing_on_chain_count": {missing},
    "orphaned_payments_count": 0,
    "amount_mismatches_count": {mismatch},
    "has_discrepancies": {has_discrepancies}
  }},
  "missing_on_chain": [],
  "orphaned_payments": [],
  "amount_mismatches": []
}}"#
                ),
            )
        }

        // ── Settlements ────────────────────────────────────────────────────────
        ("GET", "/settlements") => {
            let query = path.split_once('?').map(|(_, q)| q).unwrap_or_default();
            let params = parse_query(query);
            let limit = params
                .get("limit")
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(10);
            let direction = params
                .get("direction")
                .cloned()
                .unwrap_or_else(|| "forward".to_string());
            let _ = direction; // used for routing logic in real server

            json_response(
                200,
                &format!(
                    r#"{{
  "settlements": [
    {{
      "id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
      "asset_code": "USD",
      "total_amount": "14250.00",
      "tx_count": 47,
      "status": "completed",
      "period_start": "2026-06-01T00:00:00Z",
      "period_end": "2026-06-30T23:59:59Z",
      "created_at": "2026-06-30T06:00:00Z",
      "updated_at": "2026-06-30T06:00:00Z"
    }},
    {{
      "id": "b2c3d4e5-f6a7-8901-bcde-f12345678901",
      "asset_code": "EUR",
      "total_amount": "8730.50",
      "tx_count": 22,
      "status": "pending",
      "period_start": "2026-06-01T00:00:00Z",
      "period_end": "2026-06-30T23:59:59Z",
      "created_at": "2026-06-30T05:30:00Z",
      "updated_at": "2026-06-30T05:30:00Z"
    }}
  ],
  "next_cursor": null,
  "has_more": false
}}"#
                ),
            )
        }

        ("GET", path_ref) if path_ref.starts_with("/settlements/") => {
            let settlement_id = path_only.trim_start_matches("/settlements/");
            // 404 for the all-zeros UUID used in not-found tests
            if settlement_id == "00000000-0000-0000-0000-000000000000" {
                return json_response(404, r#"{"error":"settlement not found"}"#);
            }
            json_response(
                200,
                &format!(
                    r#"{{
  "id": "{settlement_id}",
  "asset_code": "USD",
  "total_amount": "14250.00",
  "tx_count": 47,
  "status": "completed",
  "period_start": "2026-06-01T00:00:00Z",
  "period_end": "2026-06-30T23:59:59Z",
  "created_at": "2026-06-30T06:00:00Z",
  "updated_at": "2026-06-30T06:00:00Z"
}}"#
                ),
            )
        }

        // ── Transactions ───────────────────────────────────────────────────────
        ("GET", path_ref) if path_ref.starts_with("/transactions/") => {
            let tx_id = path_only.trim_start_matches("/transactions/");
            // 404 for the all-zeros UUID used in not-found tests
            if tx_id == "00000000-0000-0000-0000-000000000000" {
                return json_response(404, r#"{"error":"transaction not found"}"#);
            }
            json_response(
                200,
                &format!(
                    r#"{{
  "id": "{tx_id}",
  "stellar_account": "GABC1234567890DEFG5678901234HIJK5678901234LMNO5678901234",
  "amount": "100.00",
  "asset_code": "USD",
  "status": "pending",
  "created_at": "2026-06-30T06:00:00Z",
  "updated_at": "2026-06-30T06:00:00Z",
  "anchor_transaction_id": null,
  "callback_type": null,
  "callback_status": null,
  "settlement_id": null,
  "memo": null,
  "memo_type": null,
  "metadata": null
}}"#
                ),
            )
        }

        _ => json_response(404, r#"{"error":"Not found"}"#),
                )
            };
            json_response(200, &body)
        }

        ("POST", "/graphql") => {
            // Consume request body (read remaining headers + body) so the client
            // does not get a broken-pipe error. For tests we just serve a fixed
            // happy-path response regardless of query content.
            json_response(
                200,
                r#"{"data":{"transactions":[{"id":"550e8400-e29b-41d4-a716-446655440000","status":"pending"}]}}"#,
            )
        }

        _ => json_response(404, r#"{"error":"Not found"}"#),
    )
}

            json_response(200, &body)
        }
        _ => json_response(404, r#"{
  "error": "Not found"
}"#),
    }
fn locks_body() -> String {
    format!(
        r#"{{
  "active_locks": [
    {{
      "resource": "settlement:550e8400-e29b-41d4-a716-446655440000",
      "token": "{SAMPLE_LOCK_TOKEN}",
      "acquired_at": 1782540612,
      "ttl_secs": 30,
      "expected_duration_secs": 30,
      "overdue": false
    }},
    {{
      "resource": "payout-batch:daily",
      "token": "89ca5ddc-51bd-44bd-817e-f4175dcab0bc",
      "acquired_at": 1782540400,
      "ttl_secs": 30,
      "expected_duration_secs": 30,
      "overdue": true
    }}
  ],
  "total": 2,
  "overdue": 1
}}"#
    )
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn json_response(status: u16, body: &str) -> String {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        _ => "OK",
    };
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
        len = body.len(),
    )
}

fn parse_query(query: &str) -> std::collections::HashMap<String, String> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Subtract `days` from a (year, month, day) date, returning a new (y, m, d).
/// Only handles the Gregorian calendar for dates near 2026. Good enough for mock data.
fn subtract_days(mut y: u32, mut m: u32, mut d: u32, mut days: u32) -> (u32, u32, u32) {
    while days > 0 {
        if d > 1 {
            d -= 1;
        } else {
            if m > 1 {
                m -= 1;
            } else {
                m = 12;
                y -= 1;
            }
            d = days_in_month(y, m);
        }
        days -= 1;
    }
    (y, m, d)
}

fn days_in_month(y: u32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}
