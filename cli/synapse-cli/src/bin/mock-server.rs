//! Mock HTTP server binary used by CLI integration tests.
//!
//! Binds to the address given in `MOCK_SERVER_ADDR` (default `127.0.0.1:4010`).
//! The `MOCK_SERVER_SCENARIO` env-var selects the response scenario:
//!
//! | Scenario     | Description                                              |
//! |--------------|-----------------------------------------------------------|
//! | `happy`      | (default) Normal responses with canned non-empty data.   |
//! | `edge`       | Edge-case responses (empty lists, session_expired, …).   |
//! | `not_found`  | 404 for every transaction/settlement id lookup.          |
//!
//! Routes served (non-exhaustive):
//!   GET  /live, /ready, /health, /errors
//!   GET  /stats/status, /stats/daily, /stats/assets, /cache/metrics
//!   POST /admin/reconciliation/run
//!   GET  /admin/reconciliation/reports[?limit=&offset=]
//!   GET  /admin/reconciliation/reports/<id>
//!   GET  /admin/locks
//!   POST /admin/transactions/bulk-status
//!   GET  /admin/webhooks/health
//!   GET  /admin/webhooks/health/<id>
//!   GET  /events
//!   POST /reconnect
//!   GET  /reconnect/status[?token=]
//!   GET  /settlements[?limit=&direction=]
//!   GET  /settlements/<id>
//!   GET  /transactions/<id>
//!   POST /graphql

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};

const SAMPLE_REPORT_ID: &str = "3f1d8c31-5f1d-4fb8-93e0-112233445566";
const SAMPLE_LOCK_TOKEN: &str = "4e4e9e47-7e0f-4f2f-8d63-323c61279209";
const SAMPLE_SESSION_ID: &str = "550e8400-e29b-41d4-a716-446655440000";
const SAMPLE_ENDPOINT_ID: &str = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";

fn main() -> std::io::Result<()> {
    let addr = std::env::var("MOCK_SERVER_ADDR").unwrap_or_else(|_| "127.0.0.1:4010".to_string());
    let scenario = std::env::var("MOCK_SERVER_SCENARIO").unwrap_or_else(|_| "happy".to_string());

    let listener = TcpListener::bind(&addr)?;
    eprintln!("Mock Synapse API listening on http://{addr} (scenario={scenario})");

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
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

    let response = route(request_line.trim_end(), scenario);
    let mut stream = stream;
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

fn route(request_line: &str, scenario: &str) -> String {
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();
    let path_only = path.split('?').next().unwrap_or(path);
    let query = path.split_once('?').map(|(_, q)| q).unwrap_or_default();

    // `not_found` scenario: every transaction/settlement id lookup 404s;
    // everything else falls back to the normal happy-path responses.
    if scenario == "not_found" {
        match (method, path_only) {
            ("GET", p) if p.starts_with("/transactions/") => {
                return json_response(404, r#"{"error":"transaction not found"}"#);
            }
            ("GET", p) if p.starts_with("/settlements/") => {
                return json_response(404, r#"{"error":"settlement not found"}"#);
            }
            _ => {}
        }
    }

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
            let params = parse_query(query);
            let days = params
                .get("days")
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(7);

            let mut rows = Vec::new();
            for i in (0..days).rev() {
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
        ("POST", "/admin/reconciliation/run") => {
            let body = if scenario == "edge" {
                run_body(false, 0, 0)
            } else {
                run_body(true, 12, 11)
            };
            json_response(200, &body)
        }

        ("GET", p)
            if p.starts_with("/admin/reconciliation/reports")
                && !p["/admin/reconciliation/reports".len()..].starts_with('/') =>
        {
            let params = parse_query(query);
            let limit = params
                .get("limit")
                .and_then(|v| v.parse::<i32>().ok())
                .unwrap_or(20);
            let offset = params
                .get("offset")
                .and_then(|v| v.parse::<i32>().ok())
                .unwrap_or(0);

            if scenario == "edge" {
                json_response(
                    200,
                    &format!(r#"{{"reports":[],"total":0,"limit":{limit},"offset":{offset}}}"#),
                )
            } else {
                json_response(
                    200,
                    &format!(
                        r#"{{"reports":[{}],"total":1,"limit":{limit},"offset":{offset}}}"#,
                        report_summary(true, 12, 11)
                    ),
                )
            }
        }

        ("GET", p) if p.starts_with("/admin/reconciliation/reports/") => {
            let report_id = p.rsplit('/').next().unwrap_or(SAMPLE_REPORT_ID);
            if scenario == "edge" {
                json_response(200, &report_detail(report_id, false, 0, 0))
            } else {
                json_response(200, &report_detail(report_id, true, 12, 11))
            }
        }

        // ── Admin / Locks ──────────────────────────────────────────────────────
        ("GET", "/admin/locks") => {
            if scenario == "edge" {
                json_response(200, r#"{"active_locks":[],"total":0,"overdue":0}"#)
            } else {
                json_response(200, &locks_body())
            }
        }

        // ── Admin / Settlements ────────────────────────────────────────────────
        ("PATCH", p) if p.starts_with("/admin/settlements/") && p.ends_with("/status") => {
            json_response(200, &settlement_status_body())
        }

        // ── Admin / Bulk transaction status ───────────────────────────────────
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

        // ── Admin / Webhooks ───────────────────────────────────────────────────
        ("GET", "/admin/webhooks/health") => json_response(200, &webhooks_health_list_body()),

        ("GET", p) if p.starts_with("/admin/webhooks/health/") => {
            let id = p.rsplit('/').next().unwrap_or_default();
            if id == SAMPLE_ENDPOINT_ID {
                json_response(200, &webhook_health_entry_body(id))
            } else {
                json_response(404, r#"{"error":"endpoint not found"}"#)
            }
        }

        // ── Events ─────────────────────────────────────────────────────────────
        ("GET", "/events") => json_response(
            200,
            r#"[
  {"transaction_id":"aaaaaaaa-0000-0000-0000-000000000001","status":"pending","timestamp":"2026-06-30T06:00:00Z","message":null},
  {"transaction_id":"aaaaaaaa-0000-0000-0000-000000000002","status":"completed","timestamp":"2026-06-30T06:05:00Z","message":"settled"}
]"#,
        ),

        ("POST", "/reconnect") | ("GET", "/reconnect/status") => {
            if scenario == "edge" {
                json_response(200, &reconnect_body_session_expired())
            } else {
                json_response(200, &reconnect_body_ready(SAMPLE_SESSION_ID))
            }
        }

        // ── Settlements ────────────────────────────────────────────────────────
        ("GET", "/settlements") => json_response(
            200,
            r#"{
  "settlements": [
    {
      "id": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
      "asset_code": "USD",
      "total_amount": "14250.00",
      "tx_count": 47,
      "status": "completed",
      "period_start": "2026-06-01T00:00:00Z",
      "period_end": "2026-06-30T23:59:59Z",
      "created_at": "2026-06-30T06:00:00Z",
      "updated_at": "2026-06-30T06:00:00Z"
    },
    {
      "id": "b2c3d4e5-f6a7-8901-bcde-f12345678901",
      "asset_code": "EUR",
      "total_amount": "8730.50",
      "tx_count": 22,
      "status": "pending",
      "period_start": "2026-06-01T00:00:00Z",
      "period_end": "2026-06-30T23:59:59Z",
      "created_at": "2026-06-30T05:30:00Z",
      "updated_at": "2026-06-30T05:30:00Z"
    }
  ],
  "next_cursor": null,
  "has_more": false
}"#,
        ),

        ("GET", p) if p.starts_with("/settlements/") => {
            let settlement_id = p.trim_start_matches("/settlements/");
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
        ("GET", p) if p.starts_with("/transactions/") => {
            let tx_id = p.trim_start_matches("/transactions/");
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

        // ── GraphQL ────────────────────────────────────────────────────────────
        ("POST", "/graphql") => json_response(
            200,
            r#"{"data":{"transactions":[{"id":"550e8400-e29b-41d4-a716-446655440000","status":"pending"}]}}"#,
        ),

        _ => json_response(404, r#"{"error":"Not found"}"#),
    }
}

// ── Body builders ─────────────────────────────────────────────────────────────

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
    )
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

fn settlement_status_body() -> String {
    r#"{
  "id": "8f9b0f0c-9a89-4d1f-9d7d-0c7d7d0d9a11",
  "asset_code": "USD",
  "total_amount": "125.0000000",
  "tx_count": 4,
  "period_start": "2026-06-01T00:00:00Z",
  "period_end": "2026-06-30T23:59:59Z",
  "status": "adjusted",
  "created_at": "2026-06-30T06:00:00Z",
  "updated_at": "2026-06-30T07:00:00Z",
  "dispute_reason": "Audit correction",
  "original_total_amount": "130.0000000",
  "reviewed_by": "admin",
  "reviewed_at": "2026-06-30T07:00:00Z"
}"#
    .to_string()
}

fn webhooks_health_list_body() -> String {
    format!(r#"[{}]"#, webhook_health_entry_body(SAMPLE_ENDPOINT_ID))
}

fn webhook_health_entry_body(id: &str) -> String {
    format!(
        r#"{{
  "id": "{id}",
  "url": "https://example.com/webhook",
  "enabled": true,
  "success_rate": 0.995,
  "total_deliveries": 200,
  "last_success_at": "2026-06-30T03:00:00Z"
}}"#
    )
}

fn reconnect_body_ready(session_id: &str) -> String {
    format!(
        r#"{{
  "type": "reconnect",
  "status": {{"status": "ready", "session_id": "{session_id}"}},
  "backoff_seconds": 5,
  "requires_resync": false
}}"#
    )
}

fn reconnect_body_session_expired() -> String {
    r#"{
  "type": "reconnect",
  "status": {"status": "session_expired"},
  "backoff_seconds": 0,
  "requires_resync": false
}"#
    .to_string()
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
            if y.is_multiple_of(4) && (!y.is_multiple_of(100) || y.is_multiple_of(400)) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}
