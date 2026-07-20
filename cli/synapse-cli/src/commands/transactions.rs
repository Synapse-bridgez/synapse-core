use crate::client::ApiClient;
use crate::formatter::{print_one, OutputFormat, TableDisplay};
use anyhow::Result;
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Response types ────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct Transaction {
    pub id: String,
    pub stellar_account: String,
    pub amount: String,
    pub asset_code: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor_transaction_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callback_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callback_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub settlement_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memo_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

// ── TableDisplay impl ─────────────────────────────────────────────────────────

impl TableDisplay for Transaction {
    fn headers() -> Vec<&'static str> {
        vec![
            "ID",
            "ASSET",
            "AMOUNT",
            "STATUS",
            "STELLAR ACCOUNT",
            "CREATED",
        ]
    }

    fn row(&self) -> Vec<String> {
        vec![
            self.id.chars().take(8).collect::<String>() + "...",
            self.asset_code.clone(),
            self.amount.clone(),
            self.status.clone(),
            // Truncate long stellar account to keep table readable
            if self.stellar_account.len() > 12 {
                format!("{}...", &self.stellar_account[..12])
            } else {
                self.stellar_account.clone()
            },
            self.created_at
                .get(..10)
                .unwrap_or(&self.created_at)
                .to_string(),
        ]
    }
}

// ── Subcommand definitions ────────────────────────────────────────────────────

#[derive(Args)]
pub struct TransactionsCmd {
    #[command(subcommand)]
    pub command: TransactionsSubcommand,
}

#[derive(Subcommand)]
pub enum TransactionsSubcommand {
    /// Get a specific transaction by ID.
    ///
    /// Calls GET /transactions/<ID>. Returns full transaction details.
    /// Exits with a non-zero status and prints an error message if the
    /// transaction is not found (404).
    ///
    /// Examples:
    ///   synapse transactions get 550e8400-e29b-41d4-a716-446655440000
    ///   synapse transactions get 550e8400-e29b-41d4-a716-446655440000 --json
    Get {
        /// Transaction UUID
        transaction_id: Uuid,

        /// Print output as JSON instead of a key-value table
        #[arg(long)]
        json: bool,
    },

    /// Export transactions with optional filters.
    ///
    /// Calls GET /transactions/export. Downloads a CSV or JSON export.
    ///
    /// Examples:
    ///   synapse transactions export
    ///   synapse transactions export --format json --status pending
    Export {
        /// Export format (csv or json)
        #[arg(long, default_value = "csv")]
        format: String,

        /// Start date filter (YYYY-MM-DD)
        #[arg(long)]
        from: Option<String>,

        /// End date filter (YYYY-MM-DD)
        #[arg(long)]
        to: Option<String>,

        /// Filter by transaction status
        #[arg(long)]
        status: Option<String>,

        /// Filter by asset code
        #[arg(long)]
        asset_code: Option<String>,

        /// Output file path (default: stdout)
        #[arg(long)]
        output: Option<String>,
    },
}

// ── Runner ────────────────────────────────────────────────────────────────────

pub async fn run(cmd: TransactionsSubcommand, base_url: &str, api_key: &str) -> Result<()> {
    let client = ApiClient::new(base_url, api_key);

    match cmd {
        TransactionsSubcommand::Get {
            transaction_id,
            json,
        } => {
            let path = format!("/transactions/{}", transaction_id);
            let tx: Transaction = client.get(&path).await?;

            let fmt = if json {
                OutputFormat::Json
            } else {
                OutputFormat::Table
            };
            print_one(&tx, fmt);
        }

        TransactionsSubcommand::Export {
            format,
            from,
            to,
            status,
            asset_code,
            output,
        } => {
            use crate::client::SynapseCliClient;
            use crate::formatter::Formatter;

            let cli_client = SynapseCliClient::new(base_url);
            let mut params: Vec<(&str, String)> = vec![("format", format.clone())];
            if let Some(ref f) = from {
                params.push(("from", f.clone()));
            }
            if let Some(ref t) = to {
                params.push(("to", t.clone()));
            }
            if let Some(ref s) = status {
                params.push(("status", s.clone()));
            }
            if let Some(ref a) = asset_code {
                params.push(("asset_code", a.clone()));
            }

            let query: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
            let bytes = cli_client.get_bytes("/transactions/export", &query).await?;
            let fmt = OutputFormat::from_format_str(&format);
            let result = Formatter::format_bytes_output(&bytes, fmt)?;

            if let Some(ref path) = output {
                std::fs::write(path, &result)?;
            } else {
                print!("{}", result);
            }
        }
    }

    Ok(())
}

// ── Unit tests (mockito) ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;

    fn sample_transaction(id: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "stellar_account": "GABC1234567890123456789012345678901234567890123456789012",
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
        })
    }

    // ── transactions get ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn transactions_get_happy_path_table() {
        let id = "550e8400-e29b-41d4-a716-446655440000";
        let mut server = Server::new_async().await;
        server
            .mock("GET", format!("/transactions/{}", id).as_str())
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(sample_transaction(id).to_string())
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let tx: Transaction = client.get(&format!("/transactions/{}", id)).await.unwrap();
        assert_eq!(tx.id, id);
        assert_eq!(tx.amount, "100.00");
        assert_eq!(tx.asset_code, "USD");
        assert_eq!(tx.status, "pending");
    }

    #[tokio::test]
    async fn transactions_get_json_mode() {
        let id = "550e8400-e29b-41d4-a716-446655440000";
        let mut server = Server::new_async().await;
        server
            .mock("GET", format!("/transactions/{}", id).as_str())
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(sample_transaction(id).to_string())
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let tx: Transaction = client.get(&format!("/transactions/{}", id)).await.unwrap();
        let json = serde_json::to_string_pretty(&tx).unwrap();
        assert!(json.contains("\"id\""));
        assert!(json.contains(id));
        assert!(json.contains("100.00"));
        assert!(json.contains("USD"));
    }

    #[tokio::test]
    async fn transactions_get_not_found_returns_err() {
        let id = "00000000-0000-0000-0000-000000000000";
        let mut server = Server::new_async().await;
        server
            .mock("GET", format!("/transactions/{}", id).as_str())
            .with_status(404)
            .with_body(r#"{"error":"transaction not found"}"#)
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let result: Result<Transaction> = client.get(&format!("/transactions/{}", id)).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("404"));
    }

    #[tokio::test]
    async fn transactions_get_server_error_returns_err() {
        let id = "550e8400-e29b-41d4-a716-446655440000";
        let mut server = Server::new_async().await;
        server
            .mock("GET", format!("/transactions/{}", id).as_str())
            .with_status(500)
            .with_body(r#"{"error":"internal server error"}"#)
            .create_async()
            .await;

        let client = ApiClient::new(&server.url(), "test-key");
        let result: Result<Transaction> = client.get(&format!("/transactions/{}", id)).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("500"));
    }

    #[tokio::test]
    async fn transactions_get_table_display_row_format() {
        let id = "550e8400-e29b-41d4-a716-446655440000";
        let tx = Transaction {
            id: id.to_string(),
            stellar_account: "GABC1234567890123456789012345678901234567890123456789012".to_string(),
            amount: "100.00".to_string(),
            asset_code: "USD".to_string(),
            status: "pending".to_string(),
            created_at: "2026-06-30T06:00:00Z".to_string(),
            updated_at: "2026-06-30T06:00:00Z".to_string(),
            anchor_transaction_id: None,
            callback_type: None,
            callback_status: None,
            settlement_id: None,
            memo: None,
            memo_type: None,
            metadata: None,
        };

        let row = tx.row();
        assert_eq!(row[0], "550e8400...");
        assert_eq!(row[1], "USD");
        assert_eq!(row[2], "100.00");
        assert_eq!(row[3], "pending");
        assert_eq!(row[4], "GABC12345678...");
        assert_eq!(row[5], "2026-06-30");
    }
}
