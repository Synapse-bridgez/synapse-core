//! HTTP liquidity source backed by a configured venue quote endpoint.
//!
//! # Venue contract
//!
//! `GET {url}?source_asset=..&destination_asset=..&amount=..` returns
//! `200 OK` with JSON `{"destination_amount": "<decimal>", "fee": "<decimal>"}`.
//! `404 Not Found` means the venue does not trade the pair. Any other non-2xx
//! status is treated as the source being unavailable.

use async_trait::async_trait;
use serde::Deserialize;
use sqlx::types::BigDecimal;

use crate::error::QuoteError;
use crate::swap::quoting::{LiquiditySource, QuoteRequest, SourceQuote};

#[derive(Debug, Deserialize)]
struct HttpQuoteResponse {
    destination_amount: BigDecimal,
    fee: BigDecimal,
}

/// A [`LiquiditySource`] that asks a venue's HTTP quote endpoint for a price.
#[derive(Debug, Clone)]
pub struct HttpLiquiditySource {
    id: String,
    url: String,
    api_key: Option<String>,
    client: reqwest::Client,
}

impl HttpLiquiditySource {
    pub fn new(id: String, url: String, api_key: Option<String>) -> Self {
        Self {
            id,
            url,
            api_key,
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl LiquiditySource for HttpLiquiditySource {
    fn id(&self) -> &str {
        &self.id
    }

    async fn quote(&self, request: &QuoteRequest) -> Result<SourceQuote, QuoteError> {
        let amount = request.amount.to_string();
        let mut builder = self.client.get(&self.url).query(&[
            ("source_asset", request.source_asset.as_str()),
            ("destination_asset", request.destination_asset.as_str()),
            ("amount", amount.as_str()),
        ]);
        if let Some(key) = &self.api_key {
            builder = builder.bearer_auth(key);
        }

        let response = builder
            .send()
            .await
            .map_err(|err| QuoteError::SourceUnavailable(self.id.clone(), err.to_string()))?;

        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(QuoteError::UnsupportedPair);
        }
        if !status.is_success() {
            return Err(QuoteError::SourceUnavailable(
                self.id.clone(),
                format!("HTTP {status}"),
            ));
        }

        let body: HttpQuoteResponse = response
            .json()
            .await
            .map_err(|_| QuoteError::InvalidSourceQuote(self.id.clone()))?;
        Ok(SourceQuote {
            destination_amount: body.destination_amount,
            fee: body.fee,
        })
    }
}
