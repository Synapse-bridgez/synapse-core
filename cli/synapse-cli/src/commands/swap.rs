use crate::client::ApiClient;
use crate::formatter::{print, print_one, OutputFormat, TableDisplay};
use anyhow::{bail, Result};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// ── Response models ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwapOrder {
    pub id: String,
    pub from_asset: String,
    pub to_asset: String,
    pub amount: String,
    pub status: String,
    #[serde(default)]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwapOrderList {
    pub orders: Vec<SwapOrder>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwapExposure {
    pub asset: String,
    pub exposure: String,
    pub limit: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwapRisk {
    pub exposures: Vec<SwapExposure>,
}

impl TableDisplay for SwapOrder {
    fn headers() -> Vec<&'static str> {
        vec!["ID", "FROM", "TO", "AMOUNT", "STATUS", "CREATED"]
    }

    fn row(&self) -> Vec<String> {
        vec![
            self.id.clone(),
            self.from_asset.clone(),
            self.to_asset.clone(),
            self.amount.clone(),
            self.status.clone(),
            self.created_at.clone().unwrap_or_default(),
        ]
    }
}

impl TableDisplay for SwapExposure {
    fn headers() -> Vec<&'static str> {
        vec!["ASSET", "EXPOSURE", "LIMIT"]
    }

    fn row(&self) -> Vec<String> {
        vec![self.asset.clone(), self.exposure.clone(), self.limit.clone()]
    }
}

// ── Subcommand definitions ────────────────────────────────────────────────────

#[derive(Args)]
pub struct SwapCmd {
    #[command(subcommand)]
    pub command: SwapSubcommand,
}

#[derive(Subcommand)]
pub enum SwapSubcommand {
    /// Create a swap order.
    ///
    /// Calls POST /swaps. The amount must be a positive decimal number.
    ///
    /// Examples:
    ///   synapse swap create --from USD --to XLM --amount 100.50
    Create {
        /// Asset code to swap from
        #[arg(long)]
        from: String,

        /// Asset code to swap to
        #[arg(long)]
        to: String,

        /// Amount of the source asset (positive decimal)
        #[arg(long)]
        amount: String,

        /// Print output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Get a swap order by ID (GET /swaps/<ID>).
    Get {
        /// Swap order UUID
        order_id: Uuid,

        /// Print output as JSON
        #[arg(long)]
        json: bool,
    },

    /// List swap orders (GET /swaps).
    List {
        /// Filter by order status
        #[arg(long)]
        status: Option<String>,

        /// Maximum number of orders to return
        #[arg(long, default_value = "20")]
        limit: u32,

        /// Print output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Cancel a swap order (POST /swaps/<ID>/cancel).
    Cancel {
        /// Swap order UUID
        order_id: Uuid,

        /// Print output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Show current swap exposure against configured limits (GET /swaps/risk).
    Risk {
        /// Print output as JSON
        #[arg(long)]
        json: bool,
    },
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Validate that `amount` is a positive decimal number (digits with an
/// optional fractional part) so malformed input fails before any request.
fn validate_amount(amount: &str) -> Result<()> {
    let mut parts = amount.splitn(2, '.');
    let int = parts.next().unwrap_or("");
    let frac = parts.next();
    let digits_ok = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    let well_formed = digits_ok(int) && frac.map_or(true, digits_ok);
    let non_zero = amount.chars().any(|c| c.is_ascii_digit() && c != '0');
    if !well_formed || !non_zero {
        bail!("invalid amount '{amount}': expected a positive decimal number such as 100 or 12.5");
    }
    Ok(())
}

fn format_for(json: bool) -> OutputFormat {
    OutputFormat::from_json_flag(json)
}

// ── Runner ────────────────────────────────────────────────────────────────────

pub async fn run(cmd: SwapSubcommand, base_url: &str, api_key: &str) -> Result<()> {
    let client = ApiClient::new(base_url, api_key);

    match cmd {
        SwapSubcommand::Create {
            from,
            to,
            amount,
            json,
        } => {
            validate_amount(&amount)?;
            let body = serde_json::json!({
                "from_asset": from,
                "to_asset": to,
                "amount": amount,
            });
            let order: SwapOrder = client.post("/swaps", body).await?;
            print_one(&order, format_for(json));
        }

        SwapSubcommand::Get { order_id, json } => {
            let order: SwapOrder = client.get(&format!("/swaps/{order_id}")).await?;
            print_one(&order, format_for(json));
        }

        SwapSubcommand::List {
            status,
            limit,
            json,
        } => {
            let limit_str = limit.to_string();
            let mut params: Vec<(&str, &str)> = vec![("limit", &limit_str)];
            if let Some(ref s) = status {
                params.push(("status", s));
            }
            let resp: SwapOrderList = client.get_query("/swaps", &params).await?;
            print(&resp.orders, format_for(json));
        }

        SwapSubcommand::Cancel { order_id, json } => {
            let resp: Value = client
                .post(&format!("/swaps/{order_id}/cancel"), serde_json::json!({}))
                .await?;
            if json {
                print_one(&resp, OutputFormat::Json);
            } else {
                println!("Swap order {order_id} cancelled");
            }
        }

        SwapSubcommand::Risk { json } => {
            let risk: SwapRisk = client.get("/swaps/risk").await?;
            print(&risk.exposures, format_for(json));
        }
    }

    Ok(())
}
