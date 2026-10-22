//! Redis-backed cache for swap quotes.
//!
//! Repeated quote requests for the same canonical request reuse a recent quote
//! instead of re-running liquidity sourcing. Each entry stores the price
//! snapshot it was derived from, so staleness is decided on read without
//! re-quoting:
//!
//! - **TTL** - an entry is rejected once `expires_at` has passed.
//! - **Price-feed update** - an entry is rejected when the latest snapshot id
//!   differs from the snapshot the quote was derived from.
//! - **Max staleness** - an entry is rejected when its snapshot is older than
//!   `max_staleness_secs`.
//!
//! Redis failures never fail a quote request: the cache is bypassed and the
//! quote is produced directly. Concurrent misses for the same key are
//! single-flighted in-process so only one upstream quote call runs.

use crate::cache::degradation::{record_redis_degraded, DegradedFallback, RedisComponent};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use opentelemetry::{metrics::Counter, KeyValue};
use redis::{aio::ConnectionManager, AsyncCommands};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::types::BigDecimal;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::Mutex as AsyncMutex;
use tracing::warn;

/// Redis key prefix for cached quotes. Bump the version when the stored shape changes.
const KEY_PREFIX: &str = "quote:v1:";

/// Tolerated clock skew between the snapshot writer and this reader. A snapshot
/// stamped further in the future than this is treated as stale.
const MAX_CLOCK_SKEW_MS: i64 = 1_000;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Direction of the requested swap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteSide {
    Buy,
    Sell,
}

impl QuoteSide {
    pub fn as_str(self) -> &'static str {
        match self {
            QuoteSide::Buy => "buy",
            QuoteSide::Sell => "sell",
        }
    }
}

/// Canonical quote request. Every field participates in the cache key.
#[derive(Debug, Clone, PartialEq)]
pub struct QuoteRequest {
    /// Tenant the quote is priced for. Keys are tenant-scoped so one tenant's
    /// pricing config can never be served to another.
    pub tenant_id: String,
    pub input_asset: String,
    pub output_asset: String,
    pub amount: BigDecimal,
    pub side: QuoteSide,
}

impl QuoteRequest {
    /// Derive the Redis key for this request. Asset codes are trimmed and
    /// upper-cased and the amount is normalized, so equivalent requests share a
    /// key. Each field is length-prefixed before hashing so no two distinct
    /// requests can serialize to the same input.
    pub fn cache_key(&self) -> String {
        let fields = [
            self.tenant_id.trim().to_string(),
            self.input_asset.trim().to_ascii_uppercase(),
            self.output_asset.trim().to_ascii_uppercase(),
            self.side.as_str().to_string(),
            self.amount.normalized().to_string(),
        ];
        let canonical: String = fields
            .iter()
            .map(|field| format!("{}:{field}", field.len()))
            .collect();
        format!(
            "{KEY_PREFIX}{}",
            hex::encode(Sha256::digest(canonical.as_bytes()))
        )
    }
}

/// Identifies the price data a quote was derived from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceSnapshot {
    pub id: String,
    /// When the snapshot was taken, by the writer's clock.
    pub taken_at: DateTime<Utc>,
}

/// A cached quote plus the metadata needed to decide whether it can be served.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedQuote<Q> {
    pub quote: Q,
    pub snapshot: PriceSnapshot,
    pub cached_at: DateTime<Utc>,
    /// Entry is not served at or after this instant.
    pub expires_at: DateTime<Utc>,
}

/// Why a cached entry was or was not served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheDecision {
    Hit,
    Expired,
    /// The price feed has moved past the snapshot the quote was derived from.
    Invalidated,
    /// The snapshot is older than the configured max staleness.
    Stale,
}

/// Decide whether `entry` can be served at `now`. Pure so it can be checked
/// without Redis or a price source.
pub fn decide<Q>(
    entry: &CachedQuote<Q>,
    latest_snapshot_id: &str,
    now: DateTime<Utc>,
    max_staleness_secs: i64,
) -> CacheDecision {
    if now >= entry.expires_at {
        return CacheDecision::Expired;
    }
    if entry.snapshot.id != latest_snapshot_id {
        return CacheDecision::Invalidated;
    }
    let age_ms = (now - entry.snapshot.taken_at).num_milliseconds();
    if age_ms < -MAX_CLOCK_SKEW_MS || age_ms > max_staleness_secs * 1_000 {
        return CacheDecision::Stale;
    }
    CacheDecision::Hit
}

/// Source of quotes and price snapshots. Implementations own the liquidity
/// sourcing; this module only caches what they return.
#[async_trait]
pub trait QuoteSource: Send + Sync {
    type Quote: Serialize + DeserializeOwned + Clone + Send + Sync;

    /// Price snapshot currently in effect for `request`.
    async fn latest_price_snapshot(
        &self,
        request: &QuoteRequest,
    ) -> Result<PriceSnapshot, BoxError>;

    /// Run liquidity sourcing. Returns the quote and the snapshot it was derived from.
    async fn quote(
        &self,
        request: &QuoteRequest,
    ) -> Result<(Self::Quote, PriceSnapshot), BoxError>;
}

#[derive(Debug, Clone)]
pub struct QuoteCacheConfig {
    /// Lifetime of a cached quote, in seconds.
    pub ttl_secs: u64,
    /// Max age of the snapshot a cached quote may be served from, in seconds.
    pub max_staleness_secs: u64,
}

enum Lookup<Q> {
    Hit(CachedQuote<Q>),
    Miss,
    StaleReject,
}

pub struct QuoteCache<S: QuoteSource> {
    redis: ConnectionManager,
    source: Arc<S>,
    config: QuoteCacheConfig,
    /// Per-key gates for single-flight quoting. Entries are removed once no
    /// caller is waiting on them.
    inflight: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
    lookups: Counter<u64>,
}

impl<S: QuoteSource> QuoteCache<S> {
    pub fn new(redis: ConnectionManager, source: Arc<S>, config: QuoteCacheConfig) -> Self {
        Self {
            redis,
            source,
            config,
            inflight: Mutex::new(HashMap::new()),
            lookups: crate::metrics::quote_cache_lookups_total(),
        }
    }

    /// Return a servable quote for `request`, from cache when possible.
    pub async fn get_or_quote(
        &self,
        request: &QuoteRequest,
    ) -> Result<CachedQuote<S::Quote>, BoxError> {
        let key = request.cache_key();

        let first = self.lookup(request, &key).await;
        let outcome = match &first {
            Lookup::Hit(_) => "hit",
            Lookup::Miss => "miss",
            Lookup::StaleReject => "stale_reject",
        };
        self.lookups.add(1, &[KeyValue::new("outcome", outcome)]);
        if let Lookup::Hit(entry) = first {
            return Ok(entry);
        }

        let gate = self.gate(&key);
        let result = {
            let _held = gate.lock().await;
            // Another caller may have filled the entry while we waited.
            match self.lookup(request, &key).await {
                Lookup::Hit(entry) => Ok(entry),
                _ => self.fill(request, &key).await,
            }
        };
        drop(gate);
        self.release(&key);
        result
    }

    async fn lookup(&self, request: &QuoteRequest, key: &str) -> Lookup<S::Quote> {
        let mut conn = self.redis.clone();
        let raw: Option<String> = match conn.get(key).await {
            Ok(raw) => raw,
            Err(e) => {
                record_redis_degraded(
                    RedisComponent::QuoteCache,
                    DegradedFallback::SkippedBestEffort,
                    &e,
                );
                return Lookup::Miss;
            }
        };
        let Some(raw) = raw else {
            return Lookup::Miss;
        };
        let entry: CachedQuote<S::Quote> = match serde_json::from_str(&raw) {
            Ok(entry) => entry,
            Err(e) => {
                warn!("Failed to deserialize cached quote: {}", e);
                return Lookup::Miss;
            }
        };

        let latest = match self.source.latest_price_snapshot(request).await {
            Ok(latest) => latest,
            Err(e) => {
                warn!("Failed to read latest price snapshot, bypassing cache: {}", e);
                return Lookup::Miss;
            }
        };

        match decide(
            &entry,
            &latest.id,
            Utc::now(),
            self.config.max_staleness_secs as i64,
        ) {
            CacheDecision::Hit => Lookup::Hit(entry),
            CacheDecision::Expired => Lookup::Miss,
            CacheDecision::Invalidated | CacheDecision::Stale => Lookup::StaleReject,
        }
    }

    async fn fill(
        &self,
        request: &QuoteRequest,
        key: &str,
    ) -> Result<CachedQuote<S::Quote>, BoxError> {
        let (quote, snapshot) = self.source.quote(request).await?;
        let cached_at = Utc::now();
        let entry = CachedQuote {
            quote,
            snapshot,
            cached_at,
            expires_at: cached_at + chrono::Duration::seconds(self.config.ttl_secs as i64),
        };
        self.store(key, &entry).await;
        Ok(entry)
    }

    async fn store(&self, key: &str, entry: &CachedQuote<S::Quote>) {
        let payload = match serde_json::to_string(entry) {
            Ok(payload) => payload,
            Err(e) => {
                warn!("Failed to serialize quote for cache: {}", e);
                return;
            }
        };
        let mut conn = self.redis.clone();
        if let Err(e) = conn
            .set_ex::<_, _, ()>(key, payload, self.config.ttl_secs as usize)
            .await
        {
            record_redis_degraded(
                RedisComponent::QuoteCache,
                DegradedFallback::SkippedBestEffort,
                &e,
            );
        }
    }

    fn gate(&self, key: &str) -> Arc<AsyncMutex<()>> {
        let mut inflight = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
        inflight
            .entry(key.to_owned())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    /// Drop the gate for `key` once only the map holds it, i.e. no caller is waiting.
    fn release(&self, key: &str) {
        let mut inflight = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(gate) = inflight.get(key) {
            if Arc::strong_count(gate) == 1 {
                inflight.remove(key);
            }
        }
    }
}
