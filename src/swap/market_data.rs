//! Market-data ingestion for swap quoting.
//!
//! Fetches prices from a [`PriceSource`], normalizes them into
//! [`PriceSnapshot`]s, caches the latest accepted snapshot in Redis, and serves
//! the last known snapshot flagged `stale` when the source is unavailable.
//! Quoting reads the cache synchronously via [`MarketDataService::latest`], so a
//! slow source never blocks request handling.
//!
//! ## Freshness semantics
//!
//! - Age is `now - source_timestamp`, clamped at zero.
//! - A snapshot is usable for quoting only when it is not `stale` and its age is
//!   at most `PRICE_MAX_AGE_SECS` (see [`PriceSnapshot::is_usable`]).
//! - Redis holds the accepted snapshot for `PRICE_CACHE_TTL_SECS` (fresh key),
//!   and a copy without expiry as the last-known fallback. A fallback copy keeps
//!   its original timestamps, so it also ages out via `PRICE_MAX_AGE_SECS`.
//! - A source timestamp more than [`CLOCK_SKEW_TOLERANCE_SECS`] ahead of
//!   ingestion is rejected with [`MarketDataError::ClockSkew`].
//!
//! ## Failure handling
//!
//! - [`MarketDataError::SourceUnavailable`] triggers the last-known fallback.
//! - Malformed or invalid payloads are logged and returned as typed errors; they
//!   never fall back, because a bad payload is not evidence of an outage.
//! - Each HTTP fetch is bounded by `PRICE_SOURCE_TIMEOUT_MS` and retried at most
//!   `PRICE_SOURCE_MAX_RETRIES` times with exponential backoff.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bigdecimal::BigDecimal;
use chrono::{DateTime, Utc};
use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use serde::Deserialize;

use crate::config::MarketDataConfig;
pub use crate::swap::quote::{AssetPair, PriceSnapshot, PriceSource};

/// How far a source timestamp may lead ingestion time before it is rejected.
pub const CLOCK_SKEW_TOLERANCE_SECS: i64 = 5;

/// Errors raised while fetching, normalizing, or caching market data.
#[derive(Debug, thiserror::Error)]
pub enum MarketDataError {
    #[error("price source '{name}' unavailable: {reason}")]
    SourceUnavailable { name: String, reason: String },
    #[error("malformed price payload: {0}")]
    MalformedPayload(String),
    #[error("invalid price: {0}")]
    InvalidPrice(String),
    #[error("source timestamp is more than 5s ahead of ingestion time")]
    ClockSkew,
    #[error("snapshots cover different pairs: {0} vs {1}")]
    PairMismatch(String, String),
    #[error("no snapshot available for {0}")]
    NoSnapshot(String),
    #[error("market data cache error: {0}")]
    Cache(String),
}

#[derive(Deserialize)]
struct PricePayload {
    price: String,
    #[serde(default)]
    bid: Option<String>,
    #[serde(default)]
    ask: Option<String>,
    timestamp: DateTime<Utc>,
}

/// Parse and normalize an upstream price payload into a [`PriceSnapshot`].
///
/// Expected JSON: `{"price": "<decimal>", "bid": "<decimal>?", "ask": "<decimal>?",
/// "timestamp": "<RFC 3339>"}`. Decimals are parsed exactly, never via `f64`.
pub fn normalize_payload(
    pair: &AssetPair,
    source: &str,
    body: &[u8],
    ingested_at: DateTime<Utc>,
) -> Result<PriceSnapshot, MarketDataError> {
    let payload: PricePayload =
        serde_json::from_slice(body).map_err(|e| MarketDataError::MalformedPayload(e.to_string()))?;

    let mid = parse_positive(&payload.price, "price")?;
    let bid = payload
        .bid
        .as_deref()
        .map(|raw| parse_positive(raw, "bid"))
        .transpose()?;
    let ask = payload
        .ask
        .as_deref()
        .map(|raw| parse_positive(raw, "ask"))
        .transpose()?;

    if let (Some(bid), Some(ask)) = (&bid, &ask) {
        if bid > ask {
            return Err(MarketDataError::InvalidPrice("bid exceeds ask".to_string()));
        }
    }

    if payload.timestamp > ingested_at + chrono::Duration::seconds(CLOCK_SKEW_TOLERANCE_SECS) {
        return Err(MarketDataError::ClockSkew);
    }

    Ok(PriceSnapshot {
        pair: pair.clone(),
        mid,
        bid,
        ask,
        source: source.to_string(),
        source_timestamp: payload.timestamp,
        ingested_at,
        stale: false,
    })
}

fn parse_positive(raw: &str, field: &str) -> Result<BigDecimal, MarketDataError> {
    let value = BigDecimal::from_str(raw.trim())
        .map_err(|e| MarketDataError::InvalidPrice(format!("{field} '{raw}': {e}")))?;
    if value <= BigDecimal::from(0) {
        return Err(MarketDataError::InvalidPrice(format!(
            "{field} must be positive"
        )));
    }
    Ok(value)
}

/// Relative difference between two snapshots' mids, in basis points of `a`.
///
/// Measurement only: callers decide whether a divergence is acceptable.
pub fn divergence_bps(
    a: &PriceSnapshot,
    b: &PriceSnapshot,
) -> Result<BigDecimal, MarketDataError> {
    if a.pair != b.pair {
        return Err(MarketDataError::PairMismatch(a.pair.key(), b.pair.key()));
    }
    if a.mid <= BigDecimal::from(0) {
        return Err(MarketDataError::InvalidPrice(
            "reference mid must be positive".to_string(),
        ));
    }
    let diff = (&a.mid - &b.mid).abs();
    Ok(diff / &a.mid * BigDecimal::from(10_000))
}

/// HTTP price source. Calls `GET {base_url}/price?base=<BASE>&quote=<QUOTE>`.
pub struct HttpPriceSource {
    name: String,
    client: reqwest::Client,
    price_url: String,
    max_retries: u32,
    retry_backoff_ms: u64,
}

impl HttpPriceSource {
    /// Build an adapter from config. Errors if `PRICE_SOURCE_URL` is unset.
    pub fn new(
        name: impl Into<String>,
        config: &MarketDataConfig,
    ) -> Result<Self, MarketDataError> {
        let name = name.into();
        let base_url = config
            .source_url
            .as_deref()
            .ok_or_else(|| MarketDataError::SourceUnavailable {
                name: name.clone(),
                reason: "PRICE_SOURCE_URL is not configured".to_string(),
            })?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(config.source_timeout_ms))
            .build()
            .map_err(|e| MarketDataError::SourceUnavailable {
                name: name.clone(),
                reason: e.to_string(),
            })?;
        Ok(Self {
            name,
            client,
            price_url: format!("{}/price", base_url.trim_end_matches('/')),
            max_retries: config.source_max_retries,
            retry_backoff_ms: config.source_retry_backoff_ms,
        })
    }

    async fn get_body(&self, pair: &AssetPair) -> Result<bytes::Bytes, String> {
        let response = self
            .client
            .get(&self.price_url)
            .query(&[("base", pair.base.as_str()), ("quote", pair.quote.as_str())])
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("HTTP {status}"));
        }
        response.bytes().await.map_err(|e| e.to_string())
    }
}

#[async_trait]
impl PriceSource for HttpPriceSource {
    fn name(&self) -> &str {
        &self.name
    }

    async fn fetch(&self, pair: &AssetPair) -> Result<PriceSnapshot, MarketDataError> {
        let mut attempt: u32 = 0;
        loop {
            match self.get_body(pair).await {
                Ok(body) => return normalize_payload(pair, &self.name, &body, Utc::now()),
                Err(reason) if attempt < self.max_retries => {
                    attempt += 1;
                    tracing::warn!(
                        source = %self.name,
                        pair = %pair.key(),
                        attempt,
                        error = %reason,
                        "price fetch failed; retrying"
                    );
                    let backoff = self.retry_backoff_ms.saturating_mul(1u64 << (attempt - 1));
                    tokio::time::sleep(Duration::from_millis(backoff)).await;
                }
                Err(reason) => {
                    return Err(MarketDataError::SourceUnavailable {
                        name: self.name.clone(),
                        reason,
                    })
                }
            }
        }
    }
}

/// Deterministic in-memory source for tests and local development.
pub struct StaticPriceSource {
    name: String,
    snapshots: HashMap<String, PriceSnapshot>,
}

impl StaticPriceSource {
    pub fn new(name: impl Into<String>, snapshots: Vec<PriceSnapshot>) -> Self {
        Self {
            name: name.into(),
            snapshots: snapshots
                .into_iter()
                .map(|s| (s.pair.key(), s))
                .collect(),
        }
    }
}

#[async_trait]
impl PriceSource for StaticPriceSource {
    fn name(&self) -> &str {
        &self.name
    }

    async fn fetch(&self, pair: &AssetPair) -> Result<PriceSnapshot, MarketDataError> {
        self.snapshots
            .get(&pair.key())
            .cloned()
            .ok_or_else(|| MarketDataError::SourceUnavailable {
                name: self.name.clone(),
                reason: format!("no price configured for {}", pair.key()),
            })
    }
}

/// Redis-backed snapshot cache. Uses `REDIS_URL` via the shared
/// `ConnectionManager` convention in `handlers::session`.
#[derive(Clone)]
pub struct PriceCache {
    conn: ConnectionManager,
    ttl_secs: u64,
}

impl PriceCache {
    pub async fn connect(redis_url: &str, ttl_secs: u64) -> Result<Self, MarketDataError> {
        let client = redis::Client::open(redis_url).map_err(cache_err)?;
        let conn = ConnectionManager::new(client).await.map_err(cache_err)?;
        Ok(Self { conn, ttl_secs })
    }

    fn fresh_key(pair: &AssetPair) -> String {
        format!("market_data:price:{}", pair.key())
    }

    fn last_known_key(pair: &AssetPair) -> String {
        format!("market_data:last_known:{}", pair.key())
    }

    /// Write the snapshot as the fresh entry (with TTL) and as the last-known fallback.
    pub async fn store(&self, snapshot: &PriceSnapshot) -> Result<(), MarketDataError> {
        let json = serde_json::to_string(snapshot).map_err(cache_err)?;
        let mut conn = self.conn.clone();
        let _: () = conn
            .set_ex(Self::fresh_key(&snapshot.pair), &json, self.ttl_secs as usize)
            .await
            .map_err(cache_err)?;
        let _: () = conn
            .set(Self::last_known_key(&snapshot.pair), &json)
            .await
            .map_err(cache_err)?;
        Ok(())
    }

    /// The cached snapshot, if it has not expired from the TTL.
    pub async fn get_fresh(
        &self,
        pair: &AssetPair,
    ) -> Result<Option<PriceSnapshot>, MarketDataError> {
        self.get(Self::fresh_key(pair)).await
    }

    /// The most recently accepted snapshot, regardless of TTL.
    pub async fn get_last_known(
        &self,
        pair: &AssetPair,
    ) -> Result<Option<PriceSnapshot>, MarketDataError> {
        self.get(Self::last_known_key(pair)).await
    }

    async fn get(&self, key: String) -> Result<Option<PriceSnapshot>, MarketDataError> {
        let mut conn = self.conn.clone();
        let raw: Option<String> = conn.get(key).await.map_err(cache_err)?;
        raw.map(|json| serde_json::from_str(&json).map_err(cache_err))
            .transpose()
    }
}

fn cache_err(e: impl std::fmt::Display) -> MarketDataError {
    MarketDataError::Cache(e.to_string())
}

/// Ingests prices from a source into the cache and serves them to the quote path.
pub struct MarketDataService {
    source: Arc<dyn PriceSource>,
    cache: PriceCache,
    max_age_secs: i64,
}

impl MarketDataService {
    pub fn new(source: Arc<dyn PriceSource>, cache: PriceCache, max_age_secs: u64) -> Self {
        Self {
            source,
            cache,
            max_age_secs: max_age_secs as i64,
        }
    }

    /// Maximum snapshot age, in seconds, that quoting may accept.
    pub fn max_age_secs(&self) -> i64 {
        self.max_age_secs
    }

    /// On-demand ingestion: fetch now and cache the result.
    ///
    /// If the source is unavailable, returns the last known snapshot with
    /// `stale = true`. Other errors are returned without fallback.
    pub async fn ingest(&self, pair: &AssetPair) -> Result<PriceSnapshot, MarketDataError> {
        match self.source.fetch(pair).await {
            Ok(snapshot) => {
                if let Err(e) = self.cache.store(&snapshot).await {
                    tracing::warn!(pair = %pair.key(), error = %e, "failed to cache price snapshot");
                }
                Ok(snapshot)
            }
            Err(err @ MarketDataError::SourceUnavailable { .. }) => {
                tracing::warn!(
                    pair = %pair.key(),
                    error = %err,
                    "price source unavailable; serving last known snapshot as stale"
                );
                let mut last_known = self
                    .cache
                    .get_last_known(pair)
                    .await?
                    .ok_or_else(|| MarketDataError::NoSnapshot(pair.key()))?;
                last_known.stale = true;
                Ok(last_known)
            }
            Err(err) => {
                tracing::error!(pair = %pair.key(), error = %err, "price ingestion failed");
                Err(err)
            }
        }
    }

    /// Read path for the quote engine. Never contacts the source.
    pub async fn latest(
        &self,
        pair: &AssetPair,
    ) -> Result<Option<PriceSnapshot>, MarketDataError> {
        self.cache.get_fresh(pair).await
    }
}

/// Ingest every pair on a fixed interval until the returned handle is aborted.
pub fn spawn_ingestion(
    service: Arc<MarketDataService>,
    pairs: Vec<AssetPair>,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            for pair in &pairs {
                // Errors are logged inside `ingest`; keep the loop running.
                let _ = service.ingest(pair).await;
            }
        }
    })
}
