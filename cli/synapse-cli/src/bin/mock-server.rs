use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};

const ADDRESS: &str = "127.0.0.1:4010";
const SAMPLE_REPORT_ID: &str = "3f1d8c31-5f1d-4fb8-93e0-112233445566";

fn main() -> std::io::Result<()> {
    let addr = std::env::var("MOCK_SERVER_ADDR").unwrap_or_else(|_| ADDRESS.to_string());
    let scenario = std::env::var("MOCK_SERVER_SCENARIO").unwrap_or_else(|_| "happy".to_string());
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
            let query = path.split_once('?').map(|(_, query)| query).unwrap_or_default();
            let params = parse_query(query);
            let limit = params.get("limit").and_then(|value| value.parse::<i32>().ok()).unwrap_or(20);
            let offset = params.get("offset").and_then(|value| value.parse::<i32>().ok()).unwrap_or(0);

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
        _ => json_response(404, r#"{
  "error": "Not found"
}"#),
    }
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
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}
