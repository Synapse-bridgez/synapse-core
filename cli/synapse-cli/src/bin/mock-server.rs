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
//!   GET   /live, /ready, /health, /errors
//!   GET   /stats/status, /stats/daily, /stats/assets, /cache/metrics
//!   POST  /admin/reconciliation/run
//!   GET   /admin/reconciliation/reports[?limit=&offset=]
//!   GET   /admin/reconciliation/reports/<id>
//!   GET   /admin/locks
//!   PATCH /admin/settlements/<id>/status
//!   PATCH /admin/transactions/bulk-status
//!   GET   /admin/webhooks/health
//!   GET   /admin/webhooks/health/<id>
//!   GET   /admin/quotas
//!   GET   /admin/quotas/<tenant_id>
//!   PUT   /admin/quotas/<tenant_id>
//!   DELETE /admin/quotas/<tenant_id>/reset
//!   GET   /events
//!   POST  /reconnect
//!   GET   /reconnect/status[?token=]
//!   GET   /settlements[?limit=&direction=]
//!   GET   /settlements/<id>
//!   GET   /transactions
//!   GET   /transactions/search
//!   GET   /transactions/<id>
//!   POST  /graphql
//!
//! # Keeping this in sync with the real server (read this before editing)
//!
//! This file hand-authors every response body independently of
//! `src/handlers/*.rs`, `src/db/queries.rs`, and the response types each CLI
//! command in `../commands/*.rs` actually deserializes into. Nothing stops
//! those from drifting apart — two real incidents motivated the automated
//! checks in the `tests` module at the bottom of this file:
//!
//!   1. The `/admin/locks` fixture didn't match what one of two independently
//!      developed test suites expected, because two features wrote
//!      conflicting mock data for the same endpoint.
//!   2. `PATCH /admin/settlements/:id/status` had no mock implementation at
//!      all until it was added by hand — a supported CLI endpoint was
//!      completely untested against the mock server.
//!
//! The `tests` module below catches both classes of bug automatically:
//!
//!   - **Fixture drift**: `mock_fixtures_deserialize_into_real_response_types`
//!     deserializes every canned response body into the real type the CLI
//!     parses it into (`synapse_cli::commands::*`, reached because this bin
//!     target implicitly links the `synapse_cli` lib), or into a type
//!     mirroring the real handler's wire shape for the couple of routes with
//!     no dedicated CLI response type (see the `TransactionListResponse` /
//!     `TransactionSearchResponse` / `GraphQlEnvelope` types in the `tests`
//!     module). A field rename or a missing required field fails the test.
//!   - **Route drift**: `every_real_route_in_scope_has_a_mock_equivalent` and
//!     `mock_server_has_no_routes_the_real_server_does_not_register` extract
//!     the real route table directly from `src/lib.rs::create_app()` and
//!     `src/handlers/admin/reconciliation.rs` (via `include_str!`, so it
//!     always reflects the current source) and compare it in both directions
//!     against `MOCK_ROUTES` below — a route added to one side and not the
//!     other fails the test by name.
//!
//! **When you add or change a route here:**
//!   1. Add/update the match arm in `route()` below.
//!   2. Add an `assert_deserializes::<...>(...)` call in
//!      `mock_fixtures_deserialize_into_real_response_types` for each
//!      scenario whose body differs, pointing at the real response type. If
//!      the type doesn't already exist in `../commands/*.rs`, either reuse it
//!      (making it `pub` if needed) or define a minimal local type in the
//!      `tests` module that mirrors the real handler's shape, with a comment
//!      citing the source file it mirrors.
//!   3. Add the `(METHOD, path)` pair to `MOCK_ROUTES` in the `tests` module.
//!   4. Update the route table in this doc comment.
//!
//! Known, intentional exception: `GET /events` is served here (for `synapse
//! admin events watch`) but has no real backing route in `create_app()` yet —
//! that's a pre-existing product gap, not mock/real drift, so it's excluded
//! from the route-parity check via `MOCK_ONLY_EXCEPTIONS` rather than papered
//! over silently.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};

const SAMPLE_REPORT_ID: &str = "3f1d8c31-5f1d-4fb8-93e0-112233445566";
const SAMPLE_LOCK_TOKEN: &str = "4e4e9e47-7e0f-4f2f-8d63-323c61279209";
const SAMPLE_SESSION_ID: &str = "550e8400-e29b-41d4-a716-446655440000";
const SAMPLE_ENDPOINT_ID: &str = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
const SAMPLE_TENANT_ID: &str = "660e8400-e29b-41d4-a716-446655440077";

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

        // ── Admin / Quotas ─────────────────────────────────────────────────────
        ("GET", "/admin/quotas") => json_response(200, &quotas_list_body()),

        ("DELETE", p) if p.starts_with("/admin/quotas/") && p.ends_with("/reset") => {
            let tenant_id = p
                .trim_start_matches("/admin/quotas/")
                .trim_end_matches("/reset")
                .trim_end_matches('/');
            json_response(
                200,
                &format!(r#"{{"message":"quota reset","tenant_id":"{tenant_id}"}}"#),
            )
        }

        ("PUT", p) if p.starts_with("/admin/quotas/") => {
            let tenant_id = p.trim_start_matches("/admin/quotas/");
            json_response(
                200,
                &format!(r#"{{"message":"quota updated","tenant_id":"{tenant_id}"}}"#),
            )
        }

        ("GET", p) if p.starts_with("/admin/quotas/") => {
            let tenant_id = p.trim_start_matches("/admin/quotas/");
            json_response(200, &quota_body(tenant_id))
        }

        // ── Admin / Bulk transaction status ───────────────────────────────────
        // Registered as PATCH in `create_app()` (src/lib.rs) — a partial update
        // of existing transactions, not a creation.
        ("PATCH", "/admin/transactions/bulk-status") => {
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
        ("GET", "/transactions") => json_response(200, &transactions_list_body()),

        // Must be checked before the `/transactions/` id-lookup prefix below,
        // since "/transactions/search" also starts with that prefix.
        ("GET", p) if p.starts_with("/transactions/search") => {
            json_response(200, &transactions_search_body())
        }

        ("GET", p) if p.starts_with("/transactions/") => {
            let tx_id = p.trim_start_matches("/transactions/");
            if tx_id == "00000000-0000-0000-0000-000000000000" {
                return json_response(404, r#"{"error":"transaction not found"}"#);
            }
            json_response(200, &transaction_body(tx_id))
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

fn quotas_list_body() -> String {
    format!(r#"[{}]"#, quota_body(SAMPLE_TENANT_ID))
}

fn quota_body(tenant_id: &str) -> String {
    format!(
        r#"{{
  "tenant_id": "{tenant_id}",
  "name": "Acme Corp",
  "rate_limit_per_minute": 120,
  "quota_status": {{"current_usage": 42, "limit": 120, "resets_at": "2026-07-21T07:00:00Z"}}
}}"#
    )
}

fn transaction_body(tx_id: &str) -> String {
    format!(
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
    )
}

fn transactions_list_body() -> String {
    format!(
        r#"{{
  "data": [{}],
  "meta": {{"next_cursor": null, "has_more": false}}
}}"#,
        transaction_body("550e8400-e29b-41d4-a716-446655440010")
    )
}

fn transactions_search_body() -> String {
    format!(
        r#"{{
  "total": 1,
  "results": [{}]
}}"#,
        transaction_body("550e8400-e29b-41d4-a716-446655440011")
    )
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

// ── Drift checks ─────────────────────────────────────────────────────────────
//
// See the module doc comment at the top of this file for why these exist and
// how to keep them green when adding or changing a route.
#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::DeserializeOwned;
    use serde::Deserialize;
    use synapse_cli::commands::{
        admin, events, health, settlements, stats, transactions, webhooks,
    };

    // A UUID used only to fill `:param` slots when probing routes that accept
    // any well-formed ID. Deliberately distinct from every sentinel ID used
    // elsewhere in this file (e.g. the all-zeros "not found" IDs).
    const GENERIC_SAMPLE_ID: &str = "550e8400-e29b-41d4-a716-446655440099";

    // ── Local types for routes with no dedicated CLI response type ────────────
    //
    // Every other route's "real type" is a `pub` response struct already
    // defined in `../commands/*.rs` — the type the CLI itself deserializes
    // into. These few routes don't have one, so these mirror the real
    // handler's wire shape directly (see the comment on each).

    /// Mirrors the envelope built by `src/handlers/webhook.rs::list_transactions_api`.
    #[allow(dead_code)]
    #[derive(Debug, Deserialize)]
    struct TransactionListResponse {
        data: Vec<transactions::Transaction>,
        meta: ListMeta,
    }

    #[allow(dead_code)]
    #[derive(Debug, Deserialize)]
    struct ListMeta {
        next_cursor: Option<String>,
        has_more: bool,
    }

    /// Mirrors the envelope built by `src/handlers/search.rs::search_transactions`.
    #[allow(dead_code)]
    #[derive(Debug, Deserialize)]
    struct TransactionSearchResponse {
        total: i64,
        #[serde(default)]
        results: Vec<transactions::Transaction>,
        #[serde(default)]
        next_cursor: Option<String>,
    }

    /// Mirrors what `src/handlers/graphql.rs::graphql_handler` always emits on
    /// success: `{"data": ...}`. That handler never adds an `"errors"` key —
    /// GraphQL-level errors there are surfaced as non-200 `AppError` bodies
    /// instead, so a bare `data` envelope is the real contract.
    #[allow(dead_code)]
    #[derive(Debug, Deserialize)]
    struct GraphQlEnvelope {
        data: serde_json::Value,
    }

    /// Minimal shape shared by every `{"error": "..."}` body this mock serves.
    #[allow(dead_code)]
    #[derive(Debug, Deserialize)]
    struct ErrorBody {
        error: String,
    }

    fn body_of(response: &str) -> &str {
        response
            .split_once("\r\n\r\n")
            .map(|(_, b)| b)
            .unwrap_or("")
    }

    fn is_404(response: &str) -> bool {
        response.starts_with("HTTP/1.1 404")
    }

    fn assert_deserializes<T: DeserializeOwned>(label: &str, response: &str) {
        let body = body_of(response);
        if let Err(err) = serde_json::from_str::<T>(body) {
            panic!(
                "{label}: mock-server fixture did not deserialize into the real \
                 response type: {err}\nbody was: {body}"
            );
        }
    }

    // ── Test 1: every fixture deserializes into its real response type ────────
    //
    // Catches incident #1 from the module doc comment: a fixture whose shape
    // silently drifted from what the real type (and therefore the real CLI
    // command) expects.
    #[test]
    fn mock_fixtures_deserialize_into_real_response_types() {
        let settlement_id = "a1b2c3d4-e5f6-7890-abcd-ef1234567890";
        let tx_id = "550e8400-e29b-41d4-a716-446655440020";
        let zero_uuid = "00000000-0000-0000-0000-000000000000";

        assert_deserializes::<health::LivenessResponse>(
            "GET /live",
            &route("GET /live HTTP/1.1", "happy"),
        );
        assert_deserializes::<health::ReadinessResponse>(
            "GET /ready",
            &route("GET /ready HTTP/1.1", "happy"),
        );
        assert_deserializes::<health::HealthStatus>(
            "GET /health",
            &route("GET /health HTTP/1.1", "happy"),
        );
        assert_deserializes::<health::ErrorCatalogResponse>(
            "GET /errors",
            &route("GET /errors HTTP/1.1", "happy"),
        );

        assert_deserializes::<Vec<stats::StatusCount>>(
            "GET /stats/status",
            &route("GET /stats/status HTTP/1.1", "happy"),
        );
        assert_deserializes::<Vec<stats::DailyTotal>>(
            "GET /stats/daily",
            &route("GET /stats/daily?days=7 HTTP/1.1", "happy"),
        );
        assert_deserializes::<Vec<stats::AssetStats>>(
            "GET /stats/assets",
            &route("GET /stats/assets HTTP/1.1", "happy"),
        );
        assert_deserializes::<stats::CacheMetrics>(
            "GET /cache/metrics",
            &route("GET /cache/metrics HTTP/1.1", "happy"),
        );

        for scenario in ["happy", "edge"] {
            assert_deserializes::<admin::RunResponse>(
                "POST /admin/reconciliation/run",
                &route("POST /admin/reconciliation/run HTTP/1.1", scenario),
            );
            assert_deserializes::<admin::ListReportsResponse>(
                "GET /admin/reconciliation/reports",
                &route("GET /admin/reconciliation/reports HTTP/1.1", scenario),
            );
            assert_deserializes::<admin::ReportDetailResponse>(
                "GET /admin/reconciliation/reports/:id",
                &route(
                    &format!("GET /admin/reconciliation/reports/{SAMPLE_REPORT_ID} HTTP/1.1"),
                    scenario,
                ),
            );
            assert_deserializes::<admin::ListLocksResponse>(
                "GET /admin/locks",
                &route("GET /admin/locks HTTP/1.1", scenario),
            );
            assert_deserializes::<admin::BulkStatusResponse>(
                "PATCH /admin/transactions/bulk-status",
                &route("PATCH /admin/transactions/bulk-status HTTP/1.1", scenario),
            );
            assert_deserializes::<events::ReconnectResponse>(
                "POST /reconnect",
                &route("POST /reconnect HTTP/1.1", scenario),
            );
            assert_deserializes::<events::ReconnectResponse>(
                "GET /reconnect/status",
                &route("GET /reconnect/status HTTP/1.1", scenario),
            );
        }

        assert_deserializes::<admin::SettlementResponse>(
            "PATCH /admin/settlements/:id/status",
            &route(
                &format!("PATCH /admin/settlements/{settlement_id}/status HTTP/1.1"),
                "happy",
            ),
        );

        assert_deserializes::<Vec<webhooks::EndpointHealth>>(
            "GET /admin/webhooks/health",
            &route("GET /admin/webhooks/health HTTP/1.1", "happy"),
        );
        assert_deserializes::<webhooks::EndpointHealth>(
            "GET /admin/webhooks/health/:id",
            &route(
                &format!("GET /admin/webhooks/health/{SAMPLE_ENDPOINT_ID} HTTP/1.1"),
                "happy",
            ),
        );

        assert_deserializes::<Vec<admin::TenantQuotaView>>(
            "GET /admin/quotas",
            &route("GET /admin/quotas HTTP/1.1", "happy"),
        );
        assert_deserializes::<admin::TenantQuotaView>(
            "GET /admin/quotas/:tenant_id",
            &route(
                &format!("GET /admin/quotas/{SAMPLE_TENANT_ID} HTTP/1.1"),
                "happy",
            ),
        );
        assert_deserializes::<serde_json::Value>(
            "PUT /admin/quotas/:tenant_id",
            &route(
                &format!("PUT /admin/quotas/{SAMPLE_TENANT_ID} HTTP/1.1"),
                "happy",
            ),
        );
        assert_deserializes::<serde_json::Value>(
            "DELETE /admin/quotas/:tenant_id/reset",
            &route(
                &format!("DELETE /admin/quotas/{SAMPLE_TENANT_ID}/reset HTTP/1.1"),
                "happy",
            ),
        );

        assert_deserializes::<Vec<admin::Event>>(
            "GET /events",
            &route("GET /events HTTP/1.1", "happy"),
        );

        assert_deserializes::<settlements::SettlementListResponse>(
            "GET /settlements",
            &route("GET /settlements HTTP/1.1", "happy"),
        );
        assert_deserializes::<settlements::Settlement>(
            "GET /settlements/:id",
            &route(
                &format!("GET /settlements/{settlement_id} HTTP/1.1"),
                "happy",
            ),
        );

        assert_deserializes::<transactions::Transaction>(
            "GET /transactions/:id",
            &route(&format!("GET /transactions/{tx_id} HTTP/1.1"), "happy"),
        );
        assert_deserializes::<TransactionListResponse>(
            "GET /transactions",
            &route("GET /transactions HTTP/1.1", "happy"),
        );
        assert_deserializes::<TransactionSearchResponse>(
            "GET /transactions/search",
            &route("GET /transactions/search HTTP/1.1", "happy"),
        );

        assert_deserializes::<GraphQlEnvelope>(
            "POST /graphql",
            &route("POST /graphql HTTP/1.1", "happy"),
        );

        // 404 sentinels: still expected to return a minimal, parseable error body.
        assert_deserializes::<ErrorBody>(
            "GET /transactions/:id (not_found scenario)",
            &route(
                &format!("GET /transactions/{zero_uuid} HTTP/1.1"),
                "not_found",
            ),
        );
        assert_deserializes::<ErrorBody>(
            "GET /settlements/:id (not_found scenario)",
            &route(
                &format!("GET /settlements/{zero_uuid} HTTP/1.1"),
                "not_found",
            ),
        );
        assert_deserializes::<ErrorBody>(
            "GET /admin/webhooks/health/:id (unknown id)",
            &route(
                &format!("GET /admin/webhooks/health/{zero_uuid} HTTP/1.1"),
                "happy",
            ),
        );
    }

    // ── Test 2: mock ⇄ real route parity ────────────────────────────────────
    //
    // Catches incident #2 from the module doc comment: a route the real
    // server registers that this mock server has never heard of (or,
    // symmetrically, a mock route the real server no longer has).
    //
    // The real side is extracted directly from `src/lib.rs::create_app()` and
    // `src/handlers/admin/reconciliation.rs` via `include_str!`, so it always
    // reflects the current route table — there is nothing to hand-update here
    // when a route is added on the real server.
    const LIB_RS: &str = include_str!("../../../../src/lib.rs");
    const RECONCILIATION_RS: &str =
        include_str!("../../../../src/handlers/admin/reconciliation.rs");

    /// Every route this mock server serves (query strings stripped). This
    /// side *is* hand-maintained — it's the documented contract of what
    /// `route()` below implements, kept in sync with the route table in the
    /// module doc comment.
    const MOCK_ROUTES: &[(&str, &str)] = &[
        ("GET", "/stats/status"),
        ("GET", "/stats/daily"),
        ("GET", "/stats/assets"),
        ("POST", "/admin/reconciliation/run"),
        ("GET", "/admin/reconciliation/reports"),
        ("GET", "/admin/reconciliation/reports/:id"),
        ("GET", "/admin/locks"),
        ("PATCH", "/admin/settlements/:id/status"),
        ("PATCH", "/admin/transactions/bulk-status"),
        ("GET", "/admin/webhooks/health"),
        ("GET", "/admin/webhooks/health/:id"),
        ("GET", "/admin/quotas"),
        ("GET", "/admin/quotas/:id"),
        ("PUT", "/admin/quotas/:id"),
        ("DELETE", "/admin/quotas/:id/reset"),
        ("GET", "/events"),
        ("POST", "/reconnect"),
        ("GET", "/reconnect/status"),
        ("GET", "/settlements"),
        ("GET", "/settlements/:id"),
        ("GET", "/transactions"),
        ("GET", "/transactions/search"),
        ("GET", "/transactions/:id"),
        ("POST", "/graphql"),
    ];

    /// Routes served here with no real backing endpoint. Each entry must
    /// carry a reason in the comment above it — this is a deliberate,
    /// reviewed exception, not a silent gap.
    const MOCK_ONLY_EXCEPTIONS: &[(&str, &str)] = &[
        // `synapse admin events watch` hits this, but `create_app()` in
        // src/lib.rs has no `/events` route at all — the CLI feature was
        // never wired up on the real server. Pre-existing product gap,
        // tracked separately from mock/real drift.
        ("GET", "/events"),
    ];

    fn extract_axum_routes(source: &str) -> Vec<(String, String)> {
        let re = regex::Regex::new(
            r#"\.route\(\s*"([^"]+)"\s*,\s*(?:axum::routing::)?(get|post|put|patch|delete)\("#,
        )
        .expect("static regex is valid");

        let mut routes = Vec::new();
        for caps in re.captures_iter(source) {
            let path = caps.get(1).unwrap().as_str().to_string();
            let method = caps.get(2).unwrap().as_str().to_uppercase();
            routes.push((method, path));
        }
        routes
    }

    fn reconciliation_nest_prefix() -> String {
        let re = regex::Regex::new(
            r#"\.nest\(\s*"([^"]+)"\s*,\s*handlers::admin::reconciliation::reconciliation_routes\(\)"#,
        )
        .expect("static regex is valid");

        let caps = re.captures(LIB_RS).unwrap_or_else(|| {
            panic!(
                "expected a `.nest(\"...\", handlers::admin::reconciliation::reconciliation_routes())` \
                 call in src/lib.rs — update this regex if that registration's shape changed"
            )
        });
        caps.get(1).unwrap().as_str().to_string()
    }

    /// True if `path` falls under one of the prefixes this drift check
    /// covers (`/admin/*`, `/stats/*`, `/settlements*`, `/transactions*`,
    /// `/events`, `/reconnect*`, `/graphql`). Everything else (health probes,
    /// `/export`, `/cache/metrics`, `/ws`, webhook/callback routes) is out of
    /// scope for route-parity, per the issue that introduced this check.
    fn in_scope(path: &str) -> bool {
        let p = path.split('?').next().unwrap_or(path);
        p == "/graphql"
            || p == "/events"
            || p.starts_with("/admin")
            || p.starts_with("/stats")
            || p.starts_with("/settlements")
            || p.starts_with("/transactions")
            || p.starts_with("/reconnect")
    }

    /// Every route `create_app()` registers, restricted to `in_scope` paths.
    fn real_routes_in_scope() -> Vec<(String, String)> {
        let prefix = reconciliation_nest_prefix();

        let mut routes = extract_axum_routes(LIB_RS);
        for entry in extract_axum_routes(RECONCILIATION_RS) {
            let method = entry.0;
            let path = entry.1;
            routes.push((method, format!("{prefix}{path}")));
        }

        routes.retain(|entry| in_scope(entry.1.as_str()));
        routes
    }

    /// Does `concrete` fit `template`, treating any `:xxx` segment in
    /// `template` as a wildcard? Segment counts must match exactly.
    fn path_matches_template(concrete: &str, template: &str) -> bool {
        let concrete_segments: Vec<&str> = concrete.split('/').collect();
        let template_segments: Vec<&str> = template.split('/').collect();
        concrete_segments.len() == template_segments.len()
            && concrete_segments
                .iter()
                .zip(template_segments.iter())
                .all(|(c, t)| t.starts_with(':') || c == t)
    }

    /// Replace every `:param` segment in `template` with a sample ID so it
    /// can be sent through `route()` directly.
    fn concretize(template: &str) -> String {
        // `/admin/webhooks/health/:id` is the one route where the mock only
        // recognizes one specific ID (it otherwise simulates a 404 "not
        // found" for unknown IDs, by design) — use that ID here so this
        // probe exercises the happy path rather than the not-found path.
        let sample = if template.starts_with("/admin/webhooks/health/") {
            SAMPLE_ENDPOINT_ID
        } else {
            GENERIC_SAMPLE_ID
        };

        template
            .split('/')
            .map(|segment| {
                if segment.starts_with(':') {
                    sample
                } else {
                    segment
                }
            })
            .collect::<Vec<_>>()
            .join("/")
    }

    #[test]
    fn every_real_route_in_scope_has_a_mock_equivalent() {
        let real = real_routes_in_scope();
        assert!(
            real.len() > 15,
            "route extraction from src/lib.rs found suspiciously few in-scope \
             routes ({}) — the regex in this test likely needs updating to \
             match a src/lib.rs refactor",
            real.len()
        );

        let mut missing = Vec::new();
        for entry in &real {
            let method = entry.0.as_str();
            let template = entry.1.as_str();
            let concrete_path = concretize(template);
            let request_line = format!("{method} {concrete_path} HTTP/1.1");
            let response = route(&request_line, "happy");
            if is_404(&response) {
                missing.push(format!("{method} {template}"));
            }
        }

        assert!(
            missing.is_empty(),
            "real routes registered in create_app() with no mock-server \
             equivalent (route() 404s for them): {missing:?}\n\
             Add a matching arm in route() above — see the module doc \
             comment for the sync policy."
        );
    }

    #[test]
    fn mock_server_has_no_routes_the_real_server_does_not_register() {
        let real = real_routes_in_scope();

        let mut untracked = Vec::new();
        for mock_entry in MOCK_ROUTES {
            let method = mock_entry.0;
            let path = mock_entry.1;

            if !in_scope(path) {
                continue;
            }
            let is_exception = MOCK_ONLY_EXCEPTIONS
                .iter()
                .any(|exception| exception.0 == method && exception.1 == path);
            if is_exception {
                continue;
            }

            let has_match = real.iter().any(|real_entry| {
                real_entry.0.as_str() == method
                    && path_matches_template(path, real_entry.1.as_str())
            });
            if !has_match {
                untracked.push(format!("{method} {path}"));
            }
        }

        assert!(
            untracked.is_empty(),
            "mock-server.rs serves routes create_app() does not register: \
             {untracked:?}\n\
             Either the real route was removed (delete the mock arm) or this \
             is an intentional mock-only route that needs a documented \
             MOCK_ONLY_EXCEPTIONS entry."
        );
    }
}
