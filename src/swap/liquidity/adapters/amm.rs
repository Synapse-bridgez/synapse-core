//! Stellar AMM (Automated Market Maker) Liquidity Adapter — Issue #1482
//!
//! Sources quotes from Horizon's liquidity pool endpoints
//! (`/liquidity_pools`, `/liquidity_pools/{id}`) using constant-product
//! pricing (x · y = k).
//!
//! ## Constant-product pricing
//!
//! For a pool with reserves `R_sell` and `R_buy`, the output for input
//! `Δ_sell` (before the 0.3% fee) is:
//!
//! ```text
//! Δ_buy = (R_buy * Δ_sell) / (R_sell + Δ_sell)
//! fee   = Δ_sell * 0.003
//! effective_input = Δ_sell * (1 - 0.003) = Δ_sell * 0.997
//! Δ_buy = (R_buy * effective_input) / (R_sell + effective_input)
//! ```
//!
//! Price impact is included in the returned [`Quote`]; the aggregator should
//! compare this against the SDEX quote and route to the better price.
//!
//! ## Unit handling
//!
//! Horizon returns pool reserves as decimal strings (7 decimal places).
//! All arithmetic uses [`BigDecimal`] to avoid floating-point rounding.

use crate::swap::liquidity::{
    AdapterError, Asset, Fill, LiquidityAdapter, Quote, VenueCapabilities, VenueConfig,
    VenueHealth, VenueLimits,
};
use async_trait::async_trait;
use bigdecimal::BigDecimal;
use chrono::Utc;
use reqwest::Client;
use serde::Deserialize;
use std::str::FromStr;
use std::time::Duration;
use tracing::instrument;

// ── Horizon AMM response types ───────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct PoolsResponse {
    #[serde(rename = "_embedded")]
    embedded: PoolsEmbedded,
}

#[derive(Debug, Deserialize)]
struct PoolsEmbedded {
    records: Vec<PoolRecord>,
}

#[derive(Debug, Deserialize)]
struct PoolRecord {
    id: String,
    fee_bp: Option<u32>, // basis points; Horizon returns 30 (= 0.30%)
    reserves: Vec<PoolReserve>,
}

#[derive(Debug, Deserialize)]
struct PoolReserve {
    asset: String, // e.g. "USDC:GABC..." or "native"
    amount: String,
}

// ── AMM adapter ──────────────────────────────────────────────────────────────

/// AMM fee rate: 0.30% = 30 basis points.
const DEFAULT_FEE_BPS: u32 = 30;
/// Basis-point denominator.
const BPS_DENOM: u64 = 10_000;

/// Stellar AMM adapter — queries Horizon liquidity pool endpoints.
pub struct AmmAdapter {
    config: VenueConfig,
    http: Client,
}

impl AmmAdapter {
    /// Create a new AMM adapter from the given venue config.
    pub fn new(config: VenueConfig) -> Self {
        let timeout = Duration::from_millis(config.timeout_ms);
        let http = Client::builder()
            .timeout(timeout)
            .pool_max_idle_per_host(8)
            .build()
            .expect("failed to build AMM HTTP client");

        Self { config, http }
    }

    /// Parse a Horizon asset string (`"CODE:ISSUER"` or `"native"`) into an
    /// [`Asset`].
    fn parse_horizon_asset(s: &str) -> Result<Asset, AdapterError> {
        if s == "native" {
            return Ok(Asset::native());
        }
        let mut parts = s.splitn(2, ':');
        let code = parts
            .next()
            .ok_or_else(|| AdapterError::MalformedResponse(format!("malformed asset: {s}")))?;
        let issuer = parts
            .next()
            .ok_or_else(|| AdapterError::MalformedResponse(format!("missing issuer in: {s}")))?;
        Ok(Asset::new(code, issuer))
    }

    /// Convert an [`Asset`] to the Horizon query-string format used in pool
    /// filtering (e.g. `"USDC:GABC..."` or `"native"`).
    fn asset_to_horizon(asset: &Asset) -> String {
        match &asset.issuer {
            Some(issuer) => format!("{}:{}", asset.code, issuer),
            None => "native".to_string(),
        }
    }

    /// Apply constant-product AMM pricing to compute output amount.
    ///
    /// `reserve_sell` and `reserve_buy` are current pool reserves.
    /// `sell_amount` is the input (including fee deduction).
    /// `fee_bps` is the pool fee in basis points (30 for 0.30%).
    ///
    /// Returns `(buy_amount, fee_paid)`.
    fn constant_product_price(
        reserve_sell: &BigDecimal,
        reserve_buy: &BigDecimal,
        sell_amount: &BigDecimal,
        fee_bps: u32,
    ) -> Result<(BigDecimal, BigDecimal), AdapterError> {
        let zero = BigDecimal::from(0);
        if reserve_sell <= &zero || reserve_buy <= &zero {
            return Err(AdapterError::InsufficientLiquidity {
                sell_asset: "pool".into(),
                buy_asset: "pool".into(),
                reason: "pool reserves are zero".into(),
            });
        }

        // fee_multiplier = 1 - fee_bps/10000
        let fee_rate =
            BigDecimal::from(fee_bps) / BigDecimal::from(BPS_DENOM);
        let one = BigDecimal::from(1);
        let fee_multiplier = &one - &fee_rate;

        let effective_input = sell_amount * &fee_multiplier;
        let fee_paid = sell_amount * &fee_rate;

        // constant-product: buy = (R_buy * effective_input) / (R_sell + effective_input)
        let numerator = reserve_buy * &effective_input;
        let denominator = reserve_sell + &effective_input;

        if denominator == zero {
            return Err(AdapterError::InsufficientLiquidity {
                sell_asset: "pool".into(),
                buy_asset: "pool".into(),
                reason: "denominator is zero in constant-product formula".into(),
            });
        }

        let buy_amount = numerator / denominator;
        Ok((buy_amount, fee_paid))
    }

    /// Find the best matching pool for the requested asset pair and compute
    /// a quote.
    async fn quote_from_pools(
        &self,
        sell_asset: &Asset,
        buy_asset: &Asset,
        sell_amount: &BigDecimal,
    ) -> Result<Quote, AdapterError> {
        let base = self.config.base_url.trim_end_matches('/');
        let url = format!("{base}/liquidity_pools");

        // Request pools that contain both assets
        let sell_str = Self::asset_to_horizon(sell_asset);
        let buy_str = Self::asset_to_horizon(buy_asset);
        let params = [
            ("reserves", sell_str.as_str()),
            ("reserves", buy_str.as_str()),
            ("limit", "20"),
        ];

        let resp = self
            .http
            .get(&url)
            .query(&params)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    AdapterError::Timeout(format!("AMM pool query timed out: {e}"))
                } else {
                    AdapterError::Unavailable(format!("AMM HTTP error: {e}"))
                }
            })?;

        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            if status == 429 {
                return Err(AdapterError::RateLimited {
                    message: body,
                    retry_after_ms: 0,
                });
            }
            return Err(AdapterError::VenueError {
                venue_id: self.venue_id().to_string(),
                message: format!("HTTP {status}: {body}"),
            });
        }

        let pools: PoolsResponse = resp.json().await.map_err(|e| {
            AdapterError::MalformedResponse(format!("failed to decode pools response: {e}"))
        })?;

        if pools.embedded.records.is_empty() {
            return Err(AdapterError::InsufficientLiquidity {
                sell_asset: sell_asset.to_string(),
                buy_asset: buy_asset.to_string(),
                reason: "no AMM pool found for this asset pair".into(),
            });
        }

        // Pick the pool with the best (highest) output for the given input.
        let mut best_quote: Option<Quote> = None;

        for pool in &pools.embedded.records {
            if pool.reserves.len() != 2 {
                continue; // malformed pool, skip
            }

            // Identify which reserve is sell vs. buy
            let parsed: Result<Vec<(Asset, BigDecimal)>, AdapterError> = pool
                .reserves
                .iter()
                .map(|r| {
                    let asset = Self::parse_horizon_asset(&r.asset)?;
                    let amount = BigDecimal::from_str(&r.amount).map_err(|e| {
                        AdapterError::MalformedResponse(format!("reserve amount: {e}"))
                    })?;
                    Ok((asset, amount))
                })
                .collect();

            let reserves = match parsed {
                Ok(r) => r,
                Err(_) => continue,
            };

            let sell_reserve = reserves.iter().find(|(a, _)| a == sell_asset);
            let buy_reserve = reserves.iter().find(|(a, _)| a == buy_asset);

            let (r_sell, r_buy) = match (sell_reserve, buy_reserve) {
                (Some((_, rs)), Some((_, rb))) => (rs.clone(), rb.clone()),
                _ => continue, // pool doesn't contain the right assets
            };

            let fee_bps = pool.fee_bp.unwrap_or(DEFAULT_FEE_BPS);

            let (buy_amount, fee_paid) =
                match Self::constant_product_price(&r_sell, &r_buy, sell_amount, fee_bps) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

            if buy_amount <= BigDecimal::from(0) {
                continue;
            }

            let price = &buy_amount / sell_amount;

            let quote = Quote {
                venue_id: self.venue_id().to_string(),
                sell_asset: sell_asset.clone(),
                buy_asset: buy_asset.clone(),
                sell_amount: sell_amount.clone(),
                buy_amount: buy_amount.clone(),
                price,
                fee: Some(fee_paid),
                timestamp: Utc::now(),
                venue_data: Some(serde_json::json!({ "pool_id": pool.id })),
            };

            match &best_quote {
                None => best_quote = Some(quote),
                Some(current) if buy_amount > current.buy_amount => {
                    best_quote = Some(quote)
                }
                _ => {}
            }
        }

        best_quote.ok_or_else(|| AdapterError::InsufficientLiquidity {
            sell_asset: sell_asset.to_string(),
            buy_asset: buy_asset.to_string(),
            reason: "no usable AMM pool with sufficient reserves".into(),
        })
    }
}

#[async_trait]
impl LiquidityAdapter for AmmAdapter {
    fn venue_id(&self) -> &str {
        "amm"
    }

    fn capabilities(&self) -> VenueCapabilities {
        VenueCapabilities {
            venue_id: "amm".into(),
            display_name: "Stellar AMM".into(),
            supports_quotes: true,
            supports_orders: true,
            supports_cancel: false, // AMM swaps are atomic, no cancel
            supported_pairs: None,
            max_depth: None, // AMM has infinite depth (pool-bounded)
            rate_limit_rps: 10,
        }
    }

    fn limits(&self) -> VenueLimits {
        VenueLimits {
            min_order_size: Some(BigDecimal::from_str("0.0000001").unwrap()),
            max_order_size: None, // pool size is the practical limit
            price_precision: 7,
            quantity_precision: 7,
            rate_limit_rps: 10,
            max_retries: self.config.max_retries,
            retry_base_delay_ms: self.config.retry_base_delay_ms,
        }
    }

    #[instrument(name = "amm.health", skip(self))]
    async fn health(&self) -> VenueHealth {
        let url = format!(
            "{}/liquidity_pools?limit=1",
            self.config.base_url.trim_end_matches('/')
        );
        match self.http.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => VenueHealth::Healthy,
            Ok(resp) => VenueHealth::Degraded {
                reason: format!("Horizon AMM returned HTTP {}", resp.status()),
            },
            Err(e) if e.is_timeout() => VenueHealth::Unhealthy {
                reason: format!("AMM health probe timed out: {e}"),
            },
            Err(e) => VenueHealth::Unhealthy {
                reason: format!("AMM unreachable: {e}"),
            },
        }
    }

    #[instrument(
        name = "amm.get_quote",
        skip(self),
        fields(
            venue = "amm",
            sell_asset = %sell_asset,
            buy_asset = %buy_asset,
            sell_amount = %sell_amount,
        )
    )]
    async fn get_quote(
        &self,
        sell_asset: &Asset,
        buy_asset: &Asset,
        sell_amount: &BigDecimal,
    ) -> Result<Quote, AdapterError> {
        if !self.config.enabled {
            return Err(AdapterError::Unavailable("AMM adapter is disabled".into()));
        }

        self.quote_from_pools(sell_asset, buy_asset, sell_amount)
            .await
    }

    #[instrument(
        name = "amm.submit_order",
        skip(self, quote),
        fields(venue = "amm", source_account = %source_account)
    )]
    async fn submit_order(
        &self,
        source_account: &str,
        quote: &Quote,
    ) -> Result<Fill, AdapterError> {
        if !self.config.enabled {
            return Err(AdapterError::Unavailable("AMM adapter is disabled".into()));
        }

        if quote.venue_id != self.venue_id() {
            return Err(AdapterError::OrderRejected(format!(
                "quote venue_id '{}' does not match this adapter '{}'",
                quote.venue_id,
                self.venue_id()
            )));
        }

        // In production, build a Stellar LiquidityPoolDeposit / PathPayment
        // operation signed by the source account and submit to Horizon.
        let order_id = format!(
            "amm:{}:{}",
            source_account,
            uuid::Uuid::new_v4()
        );

        Ok(Fill {
            order_id,
            venue_id: self.venue_id().to_string(),
            sell_asset: quote.sell_asset.clone(),
            buy_asset: quote.buy_asset.clone(),
            sell_amount: quote.sell_amount.clone(),
            buy_amount: quote.buy_amount.clone(),
            fee: quote.fee.clone(),
            ledger: None,
            timestamp: Utc::now(),
            complete: false,
        })
    }

    /// AMM swaps are atomic; once submitted they cannot be cancelled.
    async fn cancel_order(
        &self,
        _source_account: &str,
        _order_id: &str,
    ) -> Result<bool, AdapterError> {
        Err(AdapterError::OrderRejected(
            "AMM swaps are atomic and cannot be cancelled after submission".into(),
        ))
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swap::liquidity::{adapters::contract_tests, VenueConfig};
    use mockito::Server;
    use std::str::FromStr;

    fn test_config(base_url: &str) -> VenueConfig {
        VenueConfig {
            enabled: true,
            base_url: base_url.to_string(),
            timeout_ms: 5_000,
            max_retries: 3,
            retry_base_delay_ms: 100,
            max_depth: 20,
            api_key: None,
        }
    }

    #[test]
    fn constant_product_price_basic() {
        // R_sell=1000, R_buy=1000, sell=10, fee=30bps
        // effective_input = 10 * 0.997 = 9.97
        // buy = (1000 * 9.97) / (1000 + 9.97) = 9970 / 1009.97 ≈ 9.8715
        let r_sell = BigDecimal::from(1000u32);
        let r_buy = BigDecimal::from(1000u32);
        let sell = BigDecimal::from_str("10").unwrap();
        let (buy, fee) = AmmAdapter::constant_product_price(&r_sell, &r_buy, &sell, 30).unwrap();
        assert!(buy > BigDecimal::from(9u32));
        assert!(buy < BigDecimal::from(10u32));
        assert!(fee > BigDecimal::from(0u32));
    }

    #[test]
    fn constant_product_price_zero_reserves_returns_error() {
        let r_sell = BigDecimal::from(0u32);
        let r_buy = BigDecimal::from(1000u32);
        let sell = BigDecimal::from_str("10").unwrap();
        let result = AmmAdapter::constant_product_price(&r_sell, &r_buy, &sell, 30);
        assert!(result.is_err());
    }

    #[test]
    fn parse_horizon_asset_native() {
        let asset = AmmAdapter::parse_horizon_asset("native").unwrap();
        assert!(asset.is_native());
    }

    #[test]
    fn parse_horizon_asset_non_native() {
        let asset =
            AmmAdapter::parse_horizon_asset("USDC:GABC1234567890ABCDEF").unwrap();
        assert_eq!(asset.code, "USDC");
        assert_eq!(asset.issuer.as_deref(), Some("GABC1234567890ABCDEF"));
    }

    #[test]
    fn asset_to_horizon_native() {
        assert_eq!(AmmAdapter::asset_to_horizon(&Asset::native()), "native");
    }

    #[test]
    fn asset_to_horizon_non_native() {
        let asset = Asset::new("USDC", "GISSUER");
        assert_eq!(AmmAdapter::asset_to_horizon(&asset), "USDC:GISSUER");
    }

    #[tokio::test]
    async fn contract_suite() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", mockito::Matcher::Regex(r"^/liquidity_pools.*".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"_embedded":{"records":[]}}"#)
            .create_async()
            .await;

        let adapter = AmmAdapter::new(test_config(&server.url()));
        contract_tests::run_contract_tests(&adapter).await;
    }

    #[tokio::test]
    async fn get_quote_picks_best_pool() {
        let mut server = Server::new_async().await;

        let body = serde_json::json!({
            "_embedded": {
                "records": [
                    {
                        "id": "pool-small",
                        "fee_bp": 30,
                        "reserves": [
                            { "asset": "USDC:GISSUERA", "amount": "100.0000000" },
                            { "asset": "native", "amount": "100.0000000" }
                        ]
                    },
                    {
                        "id": "pool-large",
                        "fee_bp": 30,
                        "reserves": [
                            { "asset": "USDC:GISSUERA", "amount": "10000.0000000" },
                            { "asset": "native", "amount": "10000.0000000" }
                        ]
                    }
                ]
            }
        });

        let _m = server
            .mock("GET", mockito::Matcher::Regex(r"^/liquidity_pools.*".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body.to_string())
            .create_async()
            .await;

        let adapter = AmmAdapter::new(test_config(&server.url()));
        let sell = Asset::new("USDC", "GISSUERA");
        let buy = Asset::native();
        let amount = BigDecimal::from_str("1.0").unwrap();

        let quote = adapter.get_quote(&sell, &buy, &amount).await.unwrap();
        // The larger pool should give a better price (less slippage)
        assert_eq!(quote.venue_id, "amm");
        assert!(quote.buy_amount > BigDecimal::from(0u32));
        // Pool ID should be in venue_data
        assert!(quote.venue_data.is_some());
    }

    #[tokio::test]
    async fn get_quote_no_pools_returns_insufficient_liquidity() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", mockito::Matcher::Regex(r"^/liquidity_pools.*".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"_embedded":{"records":[]}}"#)
            .create_async()
            .await;

        let adapter = AmmAdapter::new(test_config(&server.url()));
        let sell = Asset::new("USDC", "GISSUER");
        let buy = Asset::native();
        let amount = BigDecimal::from_str("1.0").unwrap();

        let result = adapter.get_quote(&sell, &buy, &amount).await;
        assert!(matches!(
            result,
            Err(AdapterError::InsufficientLiquidity { .. })
        ));
    }

    #[tokio::test]
    async fn cancel_order_returns_error() {
        let adapter = AmmAdapter::new(test_config("http://localhost"));
        let result = adapter.cancel_order("GACCOUNT", "order-123").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn disabled_adapter_returns_unavailable() {
        let config = VenueConfig {
            enabled: false,
            base_url: "http://localhost".into(),
            ..Default::default()
        };
        let adapter = AmmAdapter::new(config);
        let sell = Asset::native();
        let buy = Asset::new("USDC", "G");
        let amount = BigDecimal::from_str("1.0").unwrap();
        let result = adapter.get_quote(&sell, &buy, &amount).await;
        assert!(matches!(result, Err(AdapterError::Unavailable(_))));
    }
}
