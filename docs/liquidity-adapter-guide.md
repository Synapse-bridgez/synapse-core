# Liquidity Adapter Authoring Guide

Issue #1482 — Pluggable Liquidity Provider Adapter Framework

---

## Overview

The adapter framework (`src/swap/liquidity/`) provides a stable interface over
liquidity venues so the quote aggregator can source quotes from new venues
without engine changes.  Each venue is a Rust struct that implements the
[`LiquidityAdapter`] trait.

```text
QuoteAggregator
      │
      ▼
AdapterRegistry ──► LiquidityAdapter (SDEX)
                ──► LiquidityAdapter (AMM)
                ──► LiquidityAdapter (your venue)
```

---

## Quick Start

### 1. Create your adapter module

```
src/swap/liquidity/adapters/my_venue.rs
```

### 2. Implement `LiquidityAdapter`

```rust
use async_trait::async_trait;
use crate::swap::liquidity::{
    AdapterError, Asset, Fill, LiquidityAdapter, Quote,
    VenueCapabilities, VenueConfig, VenueHealth, VenueLimits,
};
use bigdecimal::BigDecimal;

pub struct MyVenueAdapter {
    config: VenueConfig,
    http: reqwest::Client,
}

impl MyVenueAdapter {
    pub fn new(config: VenueConfig) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(config.timeout_ms))
            .build()
            .unwrap();
        Self { config, http }
    }
}

#[async_trait]
impl LiquidityAdapter for MyVenueAdapter {
    fn venue_id(&self) -> &str { "my_venue" }

    fn capabilities(&self) -> VenueCapabilities { /* … */ }
    fn limits(&self) -> VenueLimits { /* … */ }

    async fn health(&self) -> VenueHealth { /* probe venue */ }

    async fn get_quote(
        &self,
        sell_asset: &Asset,
        buy_asset: &Asset,
        sell_amount: &BigDecimal,
    ) -> Result<Quote, AdapterError> {
        if !self.config.enabled {
            return Err(AdapterError::Unavailable("disabled".into()));
        }
        // … call venue API, map response to Quote …
    }

    async fn submit_order(
        &self,
        source_account: &str,
        quote: &Quote,
    ) -> Result<Fill, AdapterError> { /* … */ }

    async fn cancel_order(
        &self,
        source_account: &str,
        order_id: &str,
    ) -> Result<bool, AdapterError> { /* … */ }
}
```

### 3. Export from the adapters mod

In `src/swap/liquidity/adapters/mod.rs`:

```rust
pub mod my_venue;
```

### 4. Pass the contract tests

In your module's `#[cfg(test)]` block:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::swap::liquidity::adapters::contract_tests;

    #[tokio::test]
    async fn contract_suite() {
        let adapter = MyVenueAdapter::new(test_config());
        contract_tests::run_contract_tests(&adapter).await;
    }
}
```

### 5. Add environment-variable config

Add your venue's variables to `src/config.rs` → `VenueConfigs::from_env()`:

```rust
my_venue: VenueConfig {
    enabled: env::var("MY_VENUE_ENABLED")
        .map(|v| v == "true").unwrap_or(false),
    base_url: env::var("MY_VENUE_BASE_URL").unwrap_or_default(),
    // …
},
```

And register it in `VenueConfigs::build_registry()`:

```rust
if self.my_venue.enabled {
    registry.register(Arc::new(
        MyVenueAdapter::new(self.my_venue.to_liquidity_config())
    ));
}
```

---

## Asset Identity

All assets are represented as `Asset { code, issuer }`.  **Always include the
issuer for non-native assets.**  Omitting the issuer causes the aggregator to
treat two different USDC tokens as the same asset.

```rust
// ✅ Correct
Asset::new("USDC", "GA5ZSEJYB37JRC5AVCIA5MOP4RHTM335X2KGX3IHOJAPP5RE34K4KZVN")

// ❌ Wrong — issuer missing
Asset { code: "USDC".into(), issuer: None }
```

Native XLM:

```rust
Asset::native() // code="XLM", issuer=None
```

---

## Unit Handling

**All amounts in `Quote` and `Fill` are decimal strings (human-readable
units), never stroops.**  Horizon returns `"10.0000000"`; your adapter must
parse and return this as `BigDecimal::from_str("10.0000000")`.

Stroop conversion formula if your venue returns raw integers:
```
decimal = stroop_value / 10_000_000
```

---

## Error Taxonomy

Map all venue errors to the appropriate `AdapterError` variant so the
aggregator can decide retry vs. terminal routing:

| Condition | Variant | Retryable? |
|-----------|---------|-----------|
| Venue unreachable / circuit-breaker open | `Unavailable` | ✅ |
| HTTP 429 throttle | `RateLimited { retry_after_ms }` | ✅ |
| Request timed out | `Timeout` | ✅ |
| Empty / insufficient order book | `InsufficientLiquidity` | ❌ |
| Asset not listed on this venue | `AssetNotSupported` | ❌ |
| Trustline missing | `TrustlineMissing` | ❌ |
| Malformed / unexpected response | `MalformedResponse` | ❌ |
| Order rejected for business reason | `OrderRejected` | ❌ |
| Unexpected catch-all | `VenueError` | ❌ |

Always include the venue ID in `VenueError`:

```rust
Err(AdapterError::VenueError {
    venue_id: self.venue_id().to_string(),
    message: format!("HTTP {status}: {body}"),
})
```

---

## Reliability Requirements

Every adapter must:

1. **Respect the timeout** from `VenueConfig::timeout_ms` — pass it to
   `reqwest::Client::builder().timeout(…)`.
2. **Return `AdapterError::Unavailable`** (not panic) if `config.enabled` is
   `false`.
3. **Never retry internally** — retries are the aggregator's job.  Map
   retriable errors to the appropriate variant and return immediately.
4. **Be `Send + Sync`** — adapters are shared across async tasks.
5. **Have no mutable state** — adapters are stateless.  Metrics and circuit-
   breaker state live outside the adapter.

---

## Testing Requirements

1. **Contract tests** — `contract_tests::run_contract_tests(&adapter)` must
   pass (see `src/swap/liquidity/adapters/mod.rs`).
2. **Unit tests** with `mockito` against recorded fixture responses:
   - Happy-path quote.
   - Empty/insufficient response → `InsufficientLiquidity`.
   - HTTP 429 → `RateLimited`.
   - HTTP 5xx → `Unavailable`.
   - Malformed JSON → `MalformedResponse`.
   - Disabled adapter → `Unavailable`.
3. **Failure-injection test** for timeout:
   ```rust
   let config = VenueConfig { timeout_ms: 1, ..Default::default() };
   // server that sleeps > 1 ms
   let result = adapter.get_quote(…).await;
   assert!(matches!(result, Err(AdapterError::Timeout(_))));
   ```

---

## Reference Implementations

| Adapter | File |
|---------|------|
| Stellar SDEX | `src/swap/liquidity/adapters/sdex.rs` |
| Stellar AMM  | `src/swap/liquidity/adapters/amm.rs` |

Both implementations follow this guide exactly and serve as authoritative
references.

---

## Environment Variables

Add your venue's environment variables to `.env.example`:

```
# My Venue adapter
MY_VENUE_ENABLED=false
MY_VENUE_BASE_URL=https://api.my-venue.example.com
MY_VENUE_TIMEOUT_MS=5000
MY_VENUE_MAX_RETRIES=3
MY_VENUE_RETRY_BASE_DELAY_MS=200
MY_VENUE_MAX_DEPTH=20
MY_VENUE_API_KEY=           # inject via secret manager, never commit
```

The aggregator reads these at startup via `VenueConfigs::from_env()`.  No
credentials or URLs live in code.
