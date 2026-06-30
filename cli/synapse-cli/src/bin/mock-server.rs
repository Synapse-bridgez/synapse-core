use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};

const ADDRESS: &str = "127.0.0.1:4010";
const SAMPLE_REPORT_ID: &str = "3f1d8c31-5f1d-4fb8-93e0-112233445566";

fn main() -> std::io::Result<()> {
    let addr = std::env::var("MOCK_SERVER_ADDR")
        .unwrap_or_else(|_| ADDRESS.to_string());
    let scenario = std::env::var("MOCK_SERVER_SCENARIO")
        .unwrap_or_else(|_| "happy".to_string());

    let listener = TcpListener::bind(&addr)?;
    println!("Mock Synapse API listening on http://{addr}");

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
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;

    if request_line.is_empty() {
        return Ok(());
    }

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
        // ── Admin reconciliation ──────────────────────────────────────────────

        ("POST", "/admin/reconciliation/run") => {
            let body = if scenario == "edge" {
                r#"{
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
}"#
            } else {
                r#"{
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
}"#
            };
            json_response(200, body)
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
                .unwrap_or(0);

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

        ("GET", path) if path.starts_with("/admin/reconciliation/reports/") => {
            let report_id = path.rsplit('/').next().unwrap_or(SAMPLE_REPORT_ID);

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

        // ── Stats endpoints ───────────────────────────────────────────────────

        ("GET", "/stats/status") => json_response(
            200,
            r#"[
  {"status": "pending",   "count": 42},
  {"status": "completed", "count": 198},
  {"status": "failed",    "count": 7}
]"#,
        ),

        ("GET", path) if path.starts_with("/stats/daily") => {
            let query = path.split_once('?').map(|(_, q)| q).unwrap_or_default();
            let params = parse_query(query);
            let days = params
                .get("days")
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(7);

            // Build a row per requested day (counting backward from a fixed date)
            let base_date = (2026u32, 6u32, 30u32);
            let mut rows = Vec::new();
            for i in 0..days {
                let (y, m, d) = subtract_days(base_date, i);
                rows.push(format!(
                    r#"  {{"date": "{y:04}-{m:02}-{d:02}", "total_amount": "{:.2}", "transaction_count": {}}}"#,
                    100.0 * (i + 1) as f64,
                    10 + i
                ));
            }
            let body = format!("[\n{}\n]", rows.join(",\n"));
            json_response(200, &body)
        }

        ("GET", "/stats/assets") => json_response(
            200,
            r#"[
  {"asset_code": "USD",  "total_amount": "15240.50", "transaction_count": 130},
  {"asset_code": "USDC", "total_amount": "8900.00",  "transaction_count": 75},
  {"asset_code": "EUR",  "total_amount": "3200.00",  "transaction_count": 42}
]"#,
        ),

        ("GET", "/cache/metrics") => json_response(
            200,
            r#"{
  "query_cache": {"hits": 843, "misses": 57, "size": 120},
  "idempotency_cache_hits": 612,
  "idempotency_cache_misses": 18,
  "idempotency_lock_acquired": 498,
  "idempotency_lock_contention": 3,
  "idempotency_errors": 0,
  "idempotency_fallback_count": 2
}"#,
        ),

        // ── Default ───────────────────────────────────────────────────────────

        _ => json_response(
            404,
            r#"{"error": "Not found"}"#,
        ),
    }
}

/// Naive day-subtraction for display purposes (no external dependencies needed).
///
/// Counts backward from (year, month, day) by `n` days.  Only needs to be
/// roughly correct for demo output in the README.
fn subtract_days(mut date: (u32, u32, u32), n: u32) -> (u32, u32, u32) {
    let days_in_month = [0u32, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let (mut y, mut m, mut d) = date;
    for _ in 0..n {
        if d > 1 {
            d -= 1;
        } else {
            if m > 1 {
                m -= 1;
            } else {
                y -= 1;
                m = 12;
            }
            d = days_in_month[m as usize];
        }
    }
    (y, m, d)
}

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
