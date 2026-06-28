use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};

const SAMPLE_REPORT_ID: &str = "3f1d8c31-5f1d-4fb8-93e0-112233445566";
const SAMPLE_LOCK_TOKEN: &str = "4e4e9e47-7e0f-4f2f-8d63-323c61279209";

fn main() -> std::io::Result<()> {
    let addr = std::env::var("MOCK_SERVER_ADDR").unwrap_or_else(|_| "127.0.0.1:4010".to_string());
    let scenario = std::env::var("MOCK_SERVER_SCENARIO").unwrap_or_else(|_| "happy".to_string());
    let listener = TcpListener::bind(addr)?;

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
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;

    let response = route(request_line.trim_end(), scenario);
    let mut stream = stream;
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

fn route(request_line: &str, scenario: &str) -> String {
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or_default();

    match (method, path) {
        ("POST", "/admin/reconciliation/run") => {
            if scenario == "edge" {
                json_response(200, &run_body(false, 0, 0))
            } else {
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
                json_response(
                    200,
                    &format!(
                        r#"{{
  "reports": [{}],
  "total": 1,
  "limit": {limit},
  "offset": {offset}
}}"#,
                        report_summary(true, 12, 11)
                    ),
                )
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
}

fn report_detail(report_id: &str, has_discrepancies: bool, db: i32, chain: i32) -> String {
    let missing = if has_discrepancies { 1 } else { 0 };
    let mismatch = if has_discrepancies { 1 } else { 0 };
    format!(
        r#"{{
  "id": "{report_id}",
  "generated_at": "2026-06-27T06:10:12Z",
  "period_start": "2026-06-26T06:10:12Z",
  "period_end": "2026-06-27T06:10:12Z",
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

fn json_response(status: u16, body: &str) -> String {
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
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
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}
