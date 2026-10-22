//! Liquidity Provider Adapter Framework — Issue #1482
//!
//! Defines the pluggable [`LiquidityAdapter`] trait and the [`AdapterRegistry`]
//! that the quote aggregator uses to source quotes from multiple venues without
//! knowing which venue it is talking to.
//!
//! # Architecture
//!
//! ```text
//! QuoteAggregator
//!       │
//!       ▼
//! AdapterRegistry  ──►  LiquidityAdapter (SDEX)
//!                  ──►  LiquidityAdapter (AMM)
//!                  ──►  LiquidityAdapter (future venues…)
//! ```
//!
//! Each adapter is **stateless** and constructed once from a [`VenueConfig`].
//! The registry resolves adapters by their [`VenueId`] string, so the
//! aggregator never hard-codes venue details.
//!
//! See `docs/liquidity-adapter-guide.md` for the full authoring guide.

pub mod adapters;

use async_trait::async_trait;
use bigdecimal::BigDecimal;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;

// ── Asset identity ──────────────────────────────────────────────────────────

/// Fully-qualified Stellar asset: code plus optional issuer.
///
/// Native XLM has no issuer (`issuer` is `None`).  All other assets require
/// an issuer so quote results can be matched precisely across venues.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Asset {
    /// Asset code, e.g. `"USDC"` or `"XLM"`.
    pub code: String,
    /// Issuer account ID (Stellar G-address). `None` for native XLM.
    pub issuer: Option<String>,
}

impl Asset {
    /// Create a new asset with both code and issuer (non-native assets).
    pub fn new(code: impl Into<String>, issuer: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            issuer: Some(issuer.into()),
        }
    }

    /// Create the native XLM asset.
    pub fn native() -> Self {
        Self {
            code: "XLM".into(),
            issuer: None,
        }
    }

    /// Returns `true` if this is the native XLM asset.
    pub fn is_native(&self) -> bool {
        self.issuer.is_none() && self.code == "XLM"
    }
}

impl std::fmt::Display for Asset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.issuer {
            Some(issuer) => write!(f, "{}:{}", self.code, &issuer[..8]),
            None => write!(f, "{}", self.code),
        }
    }
}

// ── Quote / Fill model ───────────────────────────────────────────────────────

/// A normalized quote returned by any adapter.
///
/// All amounts are in decimal (human-readable) units — never stroops.
/// The adapter is responsible for converting venue-native units.
#[derive(Debug, Clone)]
pub struct Quote {
    /// Venue that produced this quote.
    pub venue_id: String,
    /// Asset being sold (source side).
    pub sell_asset: Asset,
    /// Asset being bought (destination side).
    pub buy_asset: Asset,
    /// Amount of `sell_asset` to spend.
    pub sell_amount: BigDecimal,
    /// Amount of `buy_asset` received (before fees).
    pub buy_amount: BigDecimal,
    /// Implied price: `buy_amount / sell_amount`.
    pub price: BigDecimal,
    /// Total fees in `buy_asset` units, if reported by the venue.
    pub fee: Option<BigDecimal>,
    /// When the quote was retrieved (UTC).
    pub timestamp: DateTime<Utc>,
    /// Venue-specific opaque data (e.g. path, pool ID) carried through to
    /// order submission — the aggregator must not interpret this field.
    pub venue_data: Option<serde_json::Value>,
}

/// The result of submitting an order to a venue.
#[derive(Debug, Clone)]
pub struct Fill {
    /// Venue-assigned order / operation ID.
    pub order_id: String,
    /// Venue that executed the fill.
    pub venue_id: String,
    /// Asset sold.
    pub sell_asset: Asset,
    /// Asset bought.
    pub buy_asset: Asset,
    /// Amount of `sell_asset` actually consumed.
    pub sell_amount: BigDecimal,
    /// Amount of `buy_asset` actually received.
    pub buy_amount: BigDecimal,
    /// Fees paid (in `buy_asset` units), if reported.
    pub fee: Option<BigDecimal>,
    /// Ledger sequence at which the fill was confirmed.
    pub ledger: Option<u64>,
    /// Fill timestamp as reported by the venue (UTC).
    pub timestamp: DateTime<Utc>,
    /// Whether the fill is complete (`true`) or partial (`false`).
    pub complete: bool,
}

// ── Venue capabilities ───────────────────────────────────────────────────────

/// What a venue can do. Used by the aggregator for pre-flight routing.
#[derive(Debug, Clone)]
pub struct VenueCapabilities {
    /// Venue short identifier (e.g. `"sdex"`, `"amm"`).
    pub venue_id: String,
    /// Human-readable name.
    pub display_name: String,
    /// Venue supports quote retrieval.
    pub supports_quotes: bool,
    /// Venue supports order submission.
    pub supports_orders: bool,
    /// Venue supports order cancellation.
    pub supports_cancel: bool,
    /// Asset pairs actively supported. `None` means the venue accepts any
    /// pair (the aggregator must still validate at runtime).
    pub supported_pairs: Option<Vec<(Asset, Asset)>>,
    /// Maximum order book depth the venue will return per request.
    pub max_depth: Option<u32>,
    /// Maximum requests per second this venue allows (0 = unlimited).
    pub rate_limit_rps: u32,
}

/// Health snapshot for a venue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VenueHealth {
    /// Venue is reachable and accepting requests.
    Healthy,
    /// Venue is reachable but operating in a degraded state.
    Degraded { reason: String },
    /// Venue is unreachable or circuit-breaker is open.
    Unhealthy { reason: String },
}

// ── Venue limits / rate semantics ────────────────────────────────────────────

/// Per-venue operational limits sourced from config and returned to the
/// aggregator for throttle/retry decisions.
#[derive(Debug, Clone)]
pub struct VenueLimits {
    /// Minimum order size in source asset units.
    pub min_order_size: Option<BigDecimal>,
    /// Maximum order size in source asset units.
    pub max_order_size: Option<BigDecimal>,
    /// Decimal precision used by the venue (number of decimal places).
    pub price_precision: u8,
    /// Decimal precision for quantity amounts.
    pub quantity_precision: u8,
    /// Maximum requests per second before the venue throttles.
    pub rate_limit_rps: u32,
    /// Retry budget: maximum number of automatic retries on 429 / 5xx.
    pub max_retries: u32,
    /// Base delay between retries in milliseconds.
    pub retry_base_delay_ms: u64,
}

// ── Error taxonomy ───────────────────────────────────────────────────────────

/// Errors returned by any [`LiquidityAdapter`] implementation.
///
/// The aggregator uses the variant to decide retry vs. terminal failure.
#[derive(Debug, Error, Clone)]
pub enum AdapterError {
    /// Venue is unreachable or circuit-breaker open. **Retryable.**
    #[error("venue unavailable: {0}")]
    Unavailable(String),

    /// HTTP 429 — venue is throttling. **Retryable after back-off.**
    #[error("rate limited by venue (retry after {retry_after_ms}ms): {message}")]
    RateLimited {
        message: String,
        /// Milliseconds to wait before retrying, if the venue supplied a
        /// `Retry-After` header; otherwise `0` (use exponential back-off).
        retry_after_ms: u64,
    },

    /// Order book is empty or does not contain enough depth for the
    /// requested size. **Not retryable** without waiting for book change.
    #[error("insufficient liquidity for {sell_asset}/{buy_asset}: {reason}")]
    InsufficientLiquidity {
        sell_asset: String,
        buy_asset: String,
        reason: String,
    },

    /// Asset pair is not supported by this venue. **Terminal.**
    #[error("asset pair not supported: {sell_asset}/{buy_asset}")]
    AssetNotSupported {
        sell_asset: String,
        buy_asset: String,
    },

    /// Trustline missing for the requested asset. **Terminal** until funded.
    #[error("trustline missing for asset {asset} on account {account}")]
    TrustlineMissing { asset: String, account: String },

    /// Venue returned a malformed or unexpected response. **Terminal.**
    #[error("malformed venue response: {0}")]
    MalformedResponse(String),

    /// Request exceeded the venue's timeout budget. **Retryable.**
    #[error("venue request timed out: {0}")]
    Timeout(String),

    /// The order was rejected by the venue for a business reason. **Terminal.**
    #[error("order rejected by venue: {0}")]
    OrderRejected(String),

    /// Catch-all for unexpected venue errors. Inspect `source` for details.
    #[error("venue error ({venue_id}): {message}")]
    VenueError { venue_id: String, message: String },
}

impl AdapterError {
    /// Returns `true` if the aggregator may automatically retry this error.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            AdapterError::Unavailable(_)
                | AdapterError::RateLimited { .. }
                | AdapterError::Timeout(_)
        )
    }
}

// ── The adapter trait ────────────────────────────────────────────────────────

/// The interface every liquidity provider adapter must implement.
///
/// Adapters are **stateless** and **cloneable** — each call receives the full
/// context it needs (assets, amounts, accounts).  A single adapter instance
/// may be shared across threads; implementations must be `Send + Sync`.
///
/// ## Implementing a new adapter
///
/// See `docs/liquidity-adapter-guide.md` for the step-by-step guide.
/// The contract tests in [`adapters::contract_tests`] must pass for every
/// implementation.
#[async_trait]
pub trait LiquidityAdapter: Send + Sync {
    /// Short identifier for this venue (e.g. `"sdex"`, `"amm"`).
    /// Must be stable across restarts; used as the registry key.
    fn venue_id(&self) -> &str;

    /// Return this venue's static capabilities.
    fn capabilities(&self) -> VenueCapabilities;

    /// Return this venue's current operational limits.
    fn limits(&self) -> VenueLimits;

    /// Probe venue health.  Should be cheap (single lightweight HTTP call).
    async fn health(&self) -> VenueHealth;

    /// Retrieve a quote for swapping `sell_amount` of `sell_asset` into
    /// `buy_asset`.
    ///
    /// The returned [`Quote`] carries opaque `venue_data` that must be
    /// passed unchanged to [`submit_order`] if the aggregator decides to
    /// execute against this venue.
    async fn get_quote(
        &self,
        sell_asset: &Asset,
        buy_asset: &Asset,
        sell_amount: &BigDecimal,
    ) -> Result<Quote, AdapterError>;

    /// Submit an order using the quote returned by [`get_quote`].
    ///
    /// `source_account` is the Stellar account that will sign the transaction.
    /// `quote` must originate from this adapter (same `venue_id`).
    async fn submit_order(
        &self,
        source_account: &str,
        quote: &Quote,
    ) -> Result<Fill, AdapterError>;

    /// Cancel a previously submitted order by its venue-assigned `order_id`.
    ///
    /// Returns `Ok(true)` if the cancel was accepted, `Ok(false)` if the
    /// order no longer exists (already filled or already cancelled).
    async fn cancel_order(
        &self,
        source_account: &str,
        order_id: &str,
    ) -> Result<bool, AdapterError>;
}

// ── Registry ────────────────────────────────────────────────────────────────

/// Thread-safe registry that maps [`VenueId`] strings to adapter instances.
///
/// Construct via [`AdapterRegistry::new`] or [`AdapterRegistry::from_configs`].
/// The aggregator holds a single `Arc<AdapterRegistry>` and calls
/// [`AdapterRegistry::get`] to resolve an adapter by ID.
#[derive(Default, Clone)]
pub struct AdapterRegistry {
    adapters: HashMap<String, Arc<dyn LiquidityAdapter>>,
}

impl AdapterRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            adapters: HashMap::new(),
        }
    }

    /// Register an adapter.  Panics in debug builds if a duplicate `venue_id`
    /// is registered, to catch misconfiguration early.
    pub fn register(&mut self, adapter: Arc<dyn LiquidityAdapter>) {
        let id = adapter.venue_id().to_string();
        debug_assert!(
            !self.adapters.contains_key(&id),
            "duplicate venue_id registered: {id}"
        );
        self.adapters.insert(id, adapter);
    }

    /// Look up an adapter by its `venue_id`.
    pub fn get(&self, venue_id: &str) -> Option<Arc<dyn LiquidityAdapter>> {
        self.adapters.get(venue_id).cloned()
    }

    /// Return all registered venue IDs.
    pub fn venue_ids(&self) -> Vec<&str> {
        self.adapters.keys().map(String::as_str).collect()
    }

    /// Return the number of registered adapters.
    pub fn len(&self) -> usize {
        self.adapters.len()
    }

    /// Returns `true` if no adapters are registered.
    pub fn is_empty(&self) -> bool {
        self.adapters.is_empty()
    }

    /// Return health status for all registered venues.
    pub async fn health_all(&self) -> HashMap<String, VenueHealth> {
        let mut result = HashMap::new();
        for (id, adapter) in &self.adapters {
            result.insert(id.clone(), adapter.health().await);
        }
        result
    }
}

// ── Venue config (sourced from AppConfig) ────────────────────────────────────

/// Per-venue runtime configuration injected from environment variables.
///
/// Populated by `config::VenueConfig` (see `src/config.rs`) and passed to
/// adapter constructors, so no credentials or endpoint URLs live in code.
#[derive(Debug, Clone)]
pub struct VenueConfig {
    /// Whether this venue is enabled for new quotes/orders.
    pub enabled: bool,
    /// Base URL for this venue's API (e.g. Horizon base URL).
    pub base_url: String,
    /// Request timeout in milliseconds.
    pub timeout_ms: u64,
    /// Maximum number of retries on transient failures.
    pub max_retries: u32,
    /// Base delay between retries in milliseconds.
    pub retry_base_delay_ms: u64,
    /// Maximum order book depth to request.
    pub max_depth: u32,
    /// Per-tenant or per-venue API key, if required.
    pub api_key: Option<String>,
}

impl Default for VenueConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: String::new(),
            timeout_ms: 5_000,
            max_retries: 3,
            retry_base_delay_ms: 200,
            max_depth: 20,
            api_key: None,
        }
    }
}

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_display_native() {
        assert_eq!(Asset::native().to_string(), "XLM");
    }

    #[test]
    fn asset_display_non_native() {
        let a = Asset::new("USDC", "GABC12345678ABCDEFGH");
        assert!(a.to_string().starts_with("USDC:"));
    }

    #[test]
    fn asset_is_native() {
        assert!(Asset::native().is_native());
        assert!(!Asset::new("USDC", "GISSUER").is_native());
    }

    #[test]
    fn adapter_error_retryable() {
        assert!(AdapterError::Unavailable("x".into()).is_retryable());
        assert!(AdapterError::RateLimited {
            message: "x".into(),
            retry_after_ms: 0
        }
        .is_retryable());
        assert!(AdapterError::Timeout("x".into()).is_retryable());
        assert!(!AdapterError::AssetNotSupported {
            sell_asset: "A".into(),
            buy_asset: "B".into()
        }
        .is_retryable());
        assert!(!AdapterError::TrustlineMissing {
            asset: "A".into(),
            account: "G".into()
        }
        .is_retryable());
    }

    #[test]
    fn registry_register_and_get() {
        use std::sync::Arc;
        struct DummyAdapter;

        #[async_trait]
        impl LiquidityAdapter for DummyAdapter {
            fn venue_id(&self) -> &str {
                "dummy"
            }
            fn capabilities(&self) -> VenueCapabilities {
                VenueCapabilities {
                    venue_id: "dummy".into(),
                    display_name: "Dummy".into(),
                    supports_quotes: true,
                    supports_orders: false,
                    supports_cancel: false,
                    supported_pairs: None,
                    max_depth: None,
                    rate_limit_rps: 0,
                }
            }
            fn limits(&self) -> VenueLimits {
                VenueLimits {
                    min_order_size: None,
                    max_order_size: None,
                    price_precision: 7,
                    quantity_precision: 7,
                    rate_limit_rps: 0,
                    max_retries: 3,
                    retry_base_delay_ms: 200,
                }
            }
            async fn health(&self) -> VenueHealth {
                VenueHealth::Healthy
            }
            async fn get_quote(
                &self,
                _sell_asset: &Asset,
                _buy_asset: &Asset,
                _sell_amount: &BigDecimal,
            ) -> Result<Quote, AdapterError> {
                Err(AdapterError::Unavailable("dummy".into()))
            }
            async fn submit_order(
                &self,
                _source_account: &str,
                _quote: &Quote,
            ) -> Result<Fill, AdapterError> {
                Err(AdapterError::Unavailable("dummy".into()))
            }
            async fn cancel_order(
                &self,
                _source_account: &str,
                _order_id: &str,
            ) -> Result<bool, AdapterError> {
                Err(AdapterError::Unavailable("dummy".into()))
            }
        }

        let mut registry = AdapterRegistry::new();
        registry.register(Arc::new(DummyAdapter));
        assert_eq!(registry.len(), 1);
        assert!(registry.get("dummy").is_some());
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn registry_empty() {
        let r = AdapterRegistry::new();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
    }
}
