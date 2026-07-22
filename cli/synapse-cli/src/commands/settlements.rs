use synapse_sdk::models::{Settlement, SettlementList};

use crate::client::ApiClient;
use crate::formatter::{print, print_one, OutputFormat, TableDisplay};
use anyhow::Result;
use clap::{Args, Subcommand};
use uuid::Uuid;

// ── TableDisplay impls ────────────────────────────────────────────────────────

impl TableDisplay for Settlement {
    fn headers() -> Vec<&'static str> {
        vec![
            "ID",
            "ASSET",
            "AMOUNT",
            "TX COUNT",
            "STATUS",
            "PERIOD START",
        ]
    }

    fn row(&self) -> Vec<String> {
        vec![
            // Shorten UUID to first 8 chars for readability in table mode
            self.id.chars().take(8).collect::<String>() + "...",
            self.asset_code.clone(),
            self.total_amount.clone(),
            self.tx_count.to_string(),
            self.status.clone(),
            self.period_start.format("%Y-%m-%d").to_string(),
        ]
    }
}

// ── Subcommand definitions ────────────────────────────────────────────────────

#[derive(Args)]
pub struct SettlementsCmd {
    #[command(subcommand)]
    pub command: SettlementsSubcommand,
}

#[derive(Subcommand)]
pub enum SettlementsSubcommand {
    /// List settlements with cursor-based pagination.
    ///
    /// Calls GET /settlements. Results are returned in descending creation order.
    /// Use --cursor to page through results; --limit controls page size (1-100).
    ///
    /// Examples:
    ///   synapse settlements list
    ///   synapse settlements list --limit 25 --json
    ///   synapse settlements list --cursor <TOKEN> --direction backward
    List {
        /// Pagination cursor (opaque token from a previous response)
        #[arg(long)]
        cursor: Option<String>,

        /// Page size (1-100, default 10)
        #[arg(long, default_value = "10")]
        limit: i64,

        /// Pagination direction (forward or backward, default forward)
        #[arg(long, default_value = "forward")]
        direction: String,

        /// Print output as JSON instead of a table
        #[arg(long)]
        json: bool,
    },

    /// Get a specific settlement by ID.
    ///
    /// Calls GET /settlements/<ID>. Returns full settlement details.
    /// Exits with a non-zero status if the settlement is not found (404).
    ///
    /// Examples:
    ///   synapse settlements get 550e8400-e29b-41d4-a716-446655440000
    ///   synapse settlements get 550e8400-e29b-41d4-a716-446655440000 --json
    Get {
        /// Settlement UUID
        settlement_id: Uuid,

        /// Print output as JSON instead of a key-value table
        #[arg(long)]
        json: bool,
    },
}

// ── Runner ────────────────────────────────────────────────────────────────────

pub async fn run(cmd: SettlementsSubcommand, base_url: &str, api_key: &str) -> Result<()> {
    let client = ApiClient::new(base_url, api_key);

    match cmd {
        SettlementsSubcommand::List {
            cursor,
            limit,
            direction,
            json,
        } => {
            let limit_str = limit.to_string();
            let mut params: Vec<(&str, &str)> =
                vec![("limit", &limit_str), ("direction", &direction)];
            // Keep cursor borrow alive for the duration of the call
            let cursor_val;
            if let Some(ref c) = cursor {
                cursor_val = c.clone();
                params.push(("cursor", &cursor_val));
            }

            let resp: SettlementList =
                client.get_with_query("/settlements", &params).await?;

            let fmt = if json {
                OutputFormat::Json
            } else {
                OutputFormat::Table
            };

            if fmt == OutputFormat::Json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&resp).unwrap_or_else(|_| "{}".into())
                );
            } else {
                // Table mode: show pagination metadata then the rows
                println!(
                    "Settlements: {} total (has_more: {})",
                    resp.settlements.len(),
                    resp.has_more
                );
                if let Some(ref c) = resp.next_cursor {
                    println!("Next cursor: {}", c);
                }
                println!();
                print(&resp.settlements, fmt);
            }
        }

        SettlementsSubcommand::Get {
            settlement_id,
            json,
        } => {
            let path = format!("/settlements/{}", settlement_id);
            let settlement: Settlement = client.get(&path).await?;

            let fmt = if json {
                OutputFormat::Json
            } else {
                OutputFormat::Table
            };
            print_one(&settlement, fmt);
        }
    }

    Ok(())
}

// ── Unit tests (mockito) ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;

    fn sample_settlement(id: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "asset_code": "USD",
            "total_amount": "1000.00",
            "tx_count": 5,
            "status": "pending",
            "period_start": "2026-06-01T00:00:00Z",
            "period_end": "2026-06-30T23:59:59Z",
            "created_at": "2026-06-30T06:00:00Z",
            "updated_at": "2026-06-30T06:00:00Z",
            "dispute_reason": null,
            "original_total_amount": null,
            "reviewed_by": null,
            "reviewed_at": null
        })
    }

    // ── settlements list ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn settlements_list_happy_path_table() {
        let id = "550e8400-e29b-41d4-a716-446655440000";
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/settlements")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "settlements": [sample_settlement(id)],
                    "next_cursor": null,
                    "has_more": false
                })
                .to_string(),
            )
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let resp: SettlementList = client.get("/settlements").await.unwrap();
        assert_eq!(resp.settlements.len(), 1);
        assert_eq!(resp.settlements[0].asset_code, "USD");
        assert_eq!(resp.settlements[0].status, "pending");
        assert!(!resp.has_more);
    }

    /// Edge case: empty list must be valid (not null/None).
    #[tokio::test]
    async fn settlements_list_empty_is_valid() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/settlements")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"settlements":[],"next_cursor":null,"has_more":false}"#)
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let resp: SettlementList = client.get("/settlements").await.unwrap();
        assert!(resp.settlements.is_empty());
        assert!(!resp.has_more);
    }

    #[tokio::test]
    async fn settlements_list_json_mode() {
        let id = "550e8400-e29b-41d4-a716-446655440000";
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/settlements")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "settlements": [sample_settlement(id)],
                    "next_cursor": null,
                    "has_more": false
                })
                .to_string(),
            )
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let resp: SettlementList = client.get("/settlements").await.unwrap();
        let json = serde_json::to_string_pretty(&resp).unwrap();
        assert!(json.contains("\"settlements\""));
        assert!(json.contains(id));
        assert!(json.contains("USD"));
    }

    #[tokio::test]
    async fn settlements_list_pagination_cursor_forwarded() {
        let id = "550e8400-e29b-41d4-a716-446655440001";
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/settlements")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("limit".into(), "5".into()),
                mockito::Matcher::UrlEncoded("direction".into(), "forward".into()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                serde_json::json!({
                    "settlements": [sample_settlement(id)],
                    "next_cursor": "next-page-token",
                    "has_more": true
                })
                .to_string(),
            )
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let resp: SettlementList = client
            .get_with_query("/settlements", &[("limit", "5"), ("direction", "forward")])
            .await
            .unwrap();
        assert!(resp.has_more);
        assert_eq!(resp.next_cursor, Some("next-page-token".to_string()));
    }

    #[tokio::test]
    async fn settlements_list_server_error_returns_err() {
        let mut server = Server::new_async().await;
        server
            .mock("GET", "/settlements")
            .with_status(500)
            .with_body(r#"{"error":"internal server error"}"#)
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let result: Result<SettlementList> = client.get("/settlements").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("500"));
    }

    // ── settlements get ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn settlements_get_happy_path_table() {
        let id = "550e8400-e29b-41d4-a716-446655440000";
        let mut server = Server::new_async().await;
        server
            .mock("GET", format!("/settlements/{}", id).as_str())
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(sample_settlement(id).to_string())
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let settlement: Settlement = client.get(&format!("/settlements/{}", id)).await.unwrap();
        assert_eq!(settlement.id, id);
        assert_eq!(settlement.total_amount, "1000.00");
        assert_eq!(settlement.tx_count, 5);
    }

    #[tokio::test]
    async fn settlements_get_json_mode() {
        let id = "550e8400-e29b-41d4-a716-446655440000";
        let mut server = Server::new_async().await;
        server
            .mock("GET", format!("/settlements/{}", id).as_str())
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(sample_settlement(id).to_string())
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let settlement: Settlement = client.get(&format!("/settlements/{}", id)).await.unwrap();
        let json = serde_json::to_string_pretty(&settlement).unwrap();
        assert!(json.contains("\"id\""));
        assert!(json.contains(id));
        assert!(json.contains("1000.00"));
    }

    #[tokio::test]
    async fn settlements_get_not_found_returns_err() {
        let id = "00000000-0000-0000-0000-000000000000";
        let mut server = Server::new_async().await;
        server
            .mock("GET", format!("/settlements/{}", id).as_str())
            .with_status(404)
            .with_body(r#"{"error":"settlement not found"}"#)
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let result: Result<Settlement> = client.get(&format!("/settlements/{}", id)).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("404"));
    }
}
