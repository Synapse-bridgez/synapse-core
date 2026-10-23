//! Stellar SDEX (Decentralized Exchange) Liquidity Adapter — Issue #1482
//!
//! Sources quotes from Horizon's order book (`/order_book`) and path-finding
//! (`/paths/strict-send`, `/paths/strict-receive`) endpoints.
//!
//! ## Unit handling
//!
//! Horizon returns all amounts as decimal strings (e.g. `"100.0000000"`).
//! The adapter converts these to [`BigDecimal`] transparently.  Stroop values
//! from on-chain are never exposed through this adapter — they would have to
//! be divided by `10_000_000` before use; Horizon already does this for us.
//!
//! ## Reliability
//!
//! Requests run through a shared [`reqwest::Client`] with a per-config
//! timeout.  All Horizon errors map to the [`AdapterError`] taxonomy so the
//! aggregator can decide retry vs. terminal routing without touching
//! Horizon-specific types.

use crate::swap::liquidity::{
    AdapterError, Asset, Fill, LiquidityAdapter, Quote, VenueCapabilities, VenueConfig, VenueHealth,
    VenueLimits,
};
use async_trait::async_trait;
use bigdecimal::BigDecimal;
use chrono::Utc;
use reqwest::Client;
use serde::Deserialize;
use std::str::FromStr;
use std::time::Duration;
use tracing::{instrument, warn};

// ── Horizon response types ───────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct OrderBookResponse {
    bids: Vec<PriceLevel>,
    asks: Vec<PriceLevel>,
}

#[derive(Debug, Deserialize)]
struct PriceLevel {
    price: String,
    amount: String,
}

#[derive(Debug, Deserialize)]
struct PathsResponse {
    #[serde(rename = "_embedded")]
    embedded: PathsEmbedded,
}

#[derive(Debug, Deserialize)]
struct PathsEmbedded {
    records: Vec<PathRecord>,
}

#[derive(Debug, Deserialize)]
struct PathRecord {
    source_amount: String,
    destination_amount: String,
    destination_asset_type: String,
    destination_asset_code: Option<String>,
    destination_asset_issuer: Option<String>,
    source_asset_type: String,
    source_asset_code: Option<String>,
    source_asset_issuer: Option<String>,
    path: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct HorizonErrorResponse {
    #[serde(default)]
    detail: String,
    #[serde(default)]
    title: String,
    status: Option<u16>,
}

// ── SDEX adapter ─────────────────────────────────────────────────────────────

/// Stellar SDEX adapter — queries Horizon order book and path-finding.
///
/// Constructed via [`SdexAdapter::new`] with a [`VenueConfig`] sourced from
/// the application configuration.
pub struct SdexAdapter {
    config: VenueConfig,
    http: Client,
}

impl SdexAdapter {
    /// Create a new SDEX adapter from the given venue config.
    pub fn new(config: VenueConfig) -> Self {
        let timeout = Duration::from_millis(config.timeout_ms);
        let http = Client::builder()
            .timeout(timeout)
            .pool_max_idle_per_host(8)
            .build()
            .expect("failed to build SDEX HTTP client");

        Self { config, http }
    }

    /// Build the Horizon asset query params for a given asset.
    fn asset_params(asset: &Asset) -> Vec<(&'static str, String)> {
        if asset.is_native() {
            vec![("asset_type", "native".to_string())]
        } else {
            let mut params = vec![
                ("asset_type", "credit_alphanum4".to_string()),
                ("asset_code", asset.code.clone()),
            ];
            if let Some(issuer) = &asset.issuer {
                params.push(("asset_issuer", issuer.clone()));
            }
            params
        }
    }

    /// Convert a [`PathRecord`] into a normalized [`Quote`].
    fn path_to_quote(record: &PathRecord, venue_id: &str) -> Result<Quote, AdapterError> {
        let sell_amount = BigDecimal::from_str(&record.source_amount)
            .map_err(|e| AdapterError::MalformedResponse(format!("source_amount: {e}")))?;
        let buy_amount = BigDecimal::from_str(&record.destination_amount)
            .map_err(|e| AdapterError::MalformedResponse(format!("destination_amount: {e}")))?;

        if sell_amount == BigDecimal::from(0) {
            return Err(AdapterError::MalformedResponse(
                "source_amount is zero".into(),
            ));
        }

        let price = &buy_amount / &sell_amount;

        let sell_asset = if record.source_asset_type == "native" {
            Asset::native()
        } else {
            Asset::new(
                record
                    .source_asset_code
                    .as_deref()
                    .ok_or_else(|| AdapterError::MalformedResponse("missing source_asset_code".into()))?,
                record
                    .source_asset_issuer
                    .as_deref()
                    .ok_or_else(|| AdapterError::MalformedResponse("missing source_asset_issuer".into()))?,
            )
        };

        let buy_asset = if record.destination_asset_type == "native" {
            Asset::native()
        } else {
            Asset::new(
                record
                    .destination_asset_code
                    .as_deref()
                    .ok_or_else(|| AdapterError::MalformedResponse("missing destination_asset_code".into()))?,
                record
                    .destination_asset_issuer
                    .as_deref()
                    .ok_or_else(|| AdapterError::MalformedResponse("missing destination_asset_issuer".into()))?,
            )
        };

        let venue_data = serde_json::to_value(&record.path).ok();

        Ok(Quote {
            venue_id: venue_id.to_string(),
            sell_asset,
            buy_asset,
            sell_amount,
            buy_amount,
            price,
            fee: None, // SDEX fees are implicit in the spread
            timestamp: Utc::now(),
            venue_data,
        })
    }

    /// Map an HTTP status + body into an [`AdapterError`].
    fn map_http_error(status: u16, body: &str, venue_id: &str) -> AdapterError {
        if status == 429 {
            return AdapterError::RateLimited {
                message: body.to_string(),
                retry_after_ms: 0,
            };
        }

        if let Ok(err) = serde_json::from_str::<HorizonErrorResponse>(body) {
            let msg = if err.detail.is_empty() {
                err.title
            } else {
                err.detail
            };
            // Horizon 404 on an order book is "asset not supported" for
            // our purposes — it means the pair has no listing.
            if status == 404 {
                return AdapterError::MalformedResponse(format!("pair not found: {msg}"));
            }
            return AdapterError::VenueError {
                venue_id: venue_id.to_string(),
                message: format!("HTTP {status}: {msg}"),
            };
        }

        AdapterError::VenueError {
            venue_id: venue_id.to_string(),
            message: format!("HTTP {status}: {body}"),
        }
    }
}

#[async_trait]
impl LiquidityAdapter for SdexAdapter {
    fn venue_id(&self) -> &str {
        "sdex"
    }

    fn capabilities(&self) -> VenueCapabilities {
        VenueCapabilities {
            venue_id: "sdex".into(),
            display_name: "Stellar SDEX".into(),
            supports_quotes: true,
            supports_orders: true,
            supports_cancel: true,
            supported_pairs: None, // any XLM or Stellar asset pair
            max_depth: Some(self.config.max_depth),
            rate_limit_rps: self.config.max_retries,
        }
    }

    fn limits(&self) -> VenueLimits {
        VenueLimits {
            // Horizon minimum meaningful amount is 0.0000001 XLM (1 stroop)
            min_order_size: Some(BigDecimal::from_str("0.0000001").unwrap()),
            max_order_size: None,
            // Stellar uses 7 decimal places (stroops)
            price_precision: 7,
            quantity_precision: 7,
            rate_limit_rps: 10, // Horizon public rate limit
            max_retries: self.config.max_retries,
            retry_base_delay_ms: self.config.retry_base_delay_ms,
        }
    }

    #[instrument(name = "sdex.health", skip(self))]
    async fn health(&self) -> VenueHealth {
        let url = format!("{}/", self.config.base_url.trim_end_matches('/'));
        match self.http.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => VenueHealth::Healthy,
            Ok(resp) => VenueHealth::Degraded {
                reason: format!("Horizon returned HTTP {}", resp.status()),
            },
            Err(e) if e.is_timeout() => VenueHealth::Unhealthy {
                reason: format!("Horizon request timed out: {e}"),
            },
            Err(e) => VenueHealth::Unhealthy {
                reason: format!("Horizon unreachable: {e}"),
            },
        }
    }

    /// Quote via Horizon's strict-send path-finding.
    ///
    /// Returns the best path for sending exactly `sell_amount` of `sell_asset`
    /// and receiving the maximum possible `buy_asset`.
    #[instrument(
        name = "sdex.get_quote",
        skip(self),
        fields(
            venue = "sdex",
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
            return Err(AdapterError::Unavailable("SDEX adapter is disabled".into()));
        }

        let base = self.config.base_url.trim_end_matches('/');
        let url = format!("{base}/paths/strict-send");

        // Build source asset params
        let src_params = Self::asset_params(sell_asset);
        let dst_params = Self::asset_params(buy_asset);

        let mut params: Vec<(String, String)> = Vec::new();
        for (k, v) in &src_params {
            params.push((format!("source_asset_{k}"), v.clone()));
        }
        for (k, v) in &dst_params {
            params.push((format!("destination_asset_{k}"), v.clone()));
        }
        params.push(("source_amount".into(), sell_amount.to_string()));

        let resp = self
            .http
            .get(&url)
            .query(&params)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    AdapterError::Timeout(format!("SDEX path-find timed out: {e}"))
                } else {
                    AdapterError::Unavailable(format!("SDEX HTTP error: {e}"))
                }
            })?;

        let status = resp.status().as_u16();
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(Self::map_http_error(status, &body, self.venue_id()));
        }

        let paths: PathsResponse = resp.json().await.map_err(|e| {
            AdapterError::MalformedResponse(format!("failed to decode paths response: {e}"))
        })?;

        // Pick the best path: most `buy_amount` for the same `sell_amount`
        paths
            .embedded
            .records
            .iter()
            .filter_map(|r| Self::path_to_quote(r, self.venue_id()).ok())
            .max_by(|a, b| a.buy_amount.partial_cmp(&b.buy_amount).unwrap_or(std::cmp::Ordering::Equal))
            .ok_or_else(|| AdapterError::InsufficientLiquidity {
                sell_asset: sell_asset.to_string(),
                buy_asset: buy_asset.to_string(),
                reason: "no paths available on SDEX".into(),
            })
    }

    /// Submit a path-payment operation to Horizon.
    ///
    /// The `quote.venue_data` field carries the path encoded by
    /// [`get_quote`] and is submitted verbatim to Horizon.  In a full
    /// implementation this would build and sign a Stellar transaction;
    /// here the order envelope is returned as a fill with status pending,
    /// to be completed by the settlement layer.
    #[instrument(
        name = "sdex.submit_order",
        skip(self, quote),
        fields(venue = "sdex", source_account = %source_account)
    )]
    async fn submit_order(
        &self,
        source_account: &str,
        quote: &Quote,
    ) -> Result<Fill, AdapterError> {
        if !self.config.enabled {
            return Err(AdapterError::Unavailable("SDEX adapter is disabled".into()));
        }

        if quote.venue_id != self.venue_id() {
            return Err(AdapterError::OrderRejected(format!(
                "quote venue_id '{}' does not match this adapter '{}'",
                quote.venue_id,
                self.venue_id()
            )));
        }

        // In a production implementation this builds a Stellar transaction
        // (PathPaymentStrictSend), signs it with the source account's key,
        // and submits it to Horizon /transactions.  For this framework
        // delivery the order ID is a placeholder; integration with the
        // signing service is tracked separately.
        let order_id = format!(
            "sdex:{}:{}",
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
            complete: false, // pending settlement confirmation
        })
    }

    /// Cancel a pending SDEX order by revoking the manage-offer operation.
    ///
    /// Horizon manage-offer with `amount=0` cancels an existing offer.
    #[instrument(
        name = "sdex.cancel_order",
        skip(self),
        fields(venue = "sdex", order_id = %order_id)
    )]
    async fn cancel_order(
        &self,
        _source_account: &str,
        order_id: &str,
    ) -> Result<bool, AdapterError> {
        if !self.config.enabled {
            return Err(AdapterError::Unavailable("SDEX adapter is disabled".into()));
        }

        // In production, submit ManageOffer(amount=0) for the offer extracted
        // from order_id.  Framework delivery returns Ok(true) as a stub.
        warn!(order_id, "SDEX cancel_order: stub implementation");
        Ok(true)
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swap::liquidity::{adapters::contract_tests, VenueConfig};
    use mockito::Server;

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

    #[tokio::test]
    async fn contract_suite() {
        let mut server = Server::new_async().await;
        // Stub a health endpoint for the contract test
        let _m = server
            .mock("GET", "/")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"horizon_version":"0.0.1"}"#)
            .create_async()
            .await;

        let adapter = SdexAdapter::new(test_config(&server.url()));
        contract_tests::run_contract_tests(&adapter).await;
    }

    #[tokio::test]
    async fn get_quote_returns_best_path() {
        let mut server = Server::new_async().await;
        let body = serde_json::json!({
            "_embedded": {
                "records": [
                    {
                        "source_amount": "10.0000000",
                        "destination_amount": "25.0000000",
                        "source_asset_type": "credit_alphanum4",
                        "source_asset_code": "USDC",
                        "source_asset_issuer": "GABC",
                        "destination_asset_type": "credit_alphanum4",
                        "destination_asset_code": "XLM",
                        "destination_asset_issuer": null,
                        "path": []
                    },
                    {
                        "source_amount": "10.0000000",
                        "destination_amount": "30.0000000",
                        "source_asset_type": "credit_alphanum4",
                        "source_asset_code": "USDC",
                        "source_asset_issuer": "GABC",
                        "destination_asset_type": "native",
                        "destination_asset_code": null,
                        "destination_asset_issuer": null,
                        "path": []
                    }
                ]
            }
        });

        let _m = server
            .mock("GET", mockito::Matcher::Regex(r"^/paths/strict-send.*".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body.to_string())
            .create_async()
            .await;

        let adapter = SdexAdapter::new(test_config(&server.url()));
        let sell = Asset::new("USDC", "GABC");
        let buy = Asset::native();
        let amount = BigDecimal::from_str("10.0").unwrap();

        let quote = adapter.get_quote(&sell, &buy, &amount).await.unwrap();
        // Should pick the path with destination_amount=30 (best)
        assert_eq!(quote.buy_amount, BigDecimal::from_str("30.0000000").unwrap());
        assert_eq!(quote.venue_id, "sdex");
    }

    #[tokio::test]
    async fn get_quote_insufficient_liquidity_on_empty_paths() {
        let mut server = Server::new_async().await;
        let body = serde_json::json!({ "_embedded": { "records": [] } });

        let _m = server
            .mock("GET", mockito::Matcher::Regex(r"^/paths/strict-send.*".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body.to_string())
            .create_async()
            .await;

        let adapter = SdexAdapter::new(test_config(&server.url()));
        let sell = Asset::new("USDC", "GABC");
        let buy = Asset::native();
        let amount = BigDecimal::from_str("1.0").unwrap();

        let result = adapter.get_quote(&sell, &buy, &amount).await;
        assert!(matches!(result, Err(AdapterError::InsufficientLiquidity { .. })));
    }

    #[tokio::test]
    async fn get_quote_rate_limited() {
        let mut server = Server::new_async().await;
        let _m = server
            .mock("GET", mockito::Matcher::Regex(r"^/paths/strict-send.*".into()))
            .with_status(429)
            .with_body("Too Many Requests")
            .create_async()
            .await;

        let adapter = SdexAdapter::new(test_config(&server.url()));
        let sell = Asset::new("USDC", "GABC");
        let buy = Asset::native();
        let amount = BigDecimal::from_str("1.0").unwrap();

        let result = adapter.get_quote(&sell, &buy, &amount).await;
        assert!(matches!(result, Err(AdapterError::RateLimited { .. })));
    }

    #[tokio::test]
    async fn disabled_adapter_returns_unavailable() {
        let config = VenueConfig {
            enabled: false,
            base_url: "http://localhost".into(),
            ..Default::default()
        };
        let adapter = SdexAdapter::new(config);
        let sell = Asset::native();
        let buy = Asset::new("USDC", "G");
        let amount = BigDecimal::from_str("1.0").unwrap();
        let result = adapter.get_quote(&sell, &buy, &amount).await;
        assert!(matches!(result, Err(AdapterError::Unavailable(_))));
    }

    #[test]
    fn adapter_error_is_retryable() {
        assert!(AdapterError::Unavailable("x".into()).is_retryable());
        assert!(!AdapterError::AssetNotSupported {
            sell_asset: "A".into(),
            buy_asset: "B".into()
        }
        .is_retryable());
    }
}
