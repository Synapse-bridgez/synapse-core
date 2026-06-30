/// Minimal HTTP mock server for CLI integration tests.
///
/// Reads MOCK_SERVER_ADDR (default 127.0.0.1:4010) and MOCK_SERVER_SCENARIO
/// (default "happy") to select which response set to serve.
///
/// Scenarios:
///   happy      — realistic success payloads for all routes (default)
///   edge       — empty/minimal payloads; reconciliation has 0 reports
///   not_found  — transactions and settlements return 404 for all IDs
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};

const DEFAULT_ADDR: &str = "127.0.0.1:4010";

fn main() -> std::io::Result<()> {
    let addr = std::env::var("MOCK_SERVER_ADDR")
        .unwrap_or_else(|_| DEFAULT_ADDR.to_string());

    let listener = TcpListener::bind(&addr)?;
    println!("Mock Synapse API listening on http://{addr}");

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(err) = handle_connection(stream) {
                    eprintln!("mock server error: {err}");
                }
            }
            Err(err) => eprintln!("mock server accept error: {err}"),
        }
    }

    Ok(())
}

fn handle_connection(stream: TcpStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
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
    let mut stream = stream;
    stream.write_all(response.as_bytes())?;
    stream.flush()?;
    Ok(())
}

/// Scenario: "not_found" — every transactions and settlements endpoint returns 404.
fn route_not_found(request_line: &str) -> String {
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
                    r#"{{
  "id": "{report_id}",
  "generated_at": "2026-06-30T06:15:00Z",
  "period_start": "2026-06-29T06:15:00Z",
  "period_end": "2026-06-30T06:15:00Z",
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
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn json_response(status: u16, body: &str) -> String {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
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
