//! Reference adapter implementations for the liquidity adapter framework.
//!
//! Each sub-module implements [`crate::swap::liquidity::LiquidityAdapter`]
//! for a specific venue.  New venue connectors must:
//!
//! 1. Add a new sub-module here.
//! 2. Pass the [`contract_tests`] suite.
//! 3. Document the adapter in `docs/liquidity-adapter-guide.md`.

pub mod amm;
pub mod sdex;

/// Uniform contract tests that every [`crate::swap::liquidity::LiquidityAdapter`]
/// implementation must pass.
///
/// Import this module in your adapter's `#[cfg(test)]` block and call
/// [`run_contract_tests`] with your adapter instance.
#[cfg(test)]
pub mod contract_tests {
    use crate::swap::liquidity::{Asset, LiquidityAdapter, VenueHealth};
    use bigdecimal::BigDecimal;
    use std::str::FromStr;

    /// Run the full contract test suite against `adapter`.
    ///
    /// # Usage
    ///
    /// ```ignore
    /// #[tokio::test]
    /// async fn contract() {
    ///     let adapter = MyAdapter::new(test_config());
    ///     contract_tests::run_contract_tests(&adapter).await;
    /// }
    /// ```
    pub async fn run_contract_tests(adapter: &dyn LiquidityAdapter) {
        test_venue_id_stable(adapter);
        test_capabilities_consistent(adapter);
        test_limits_positive(adapter);
        test_health_returns(adapter).await;
        test_unsupported_pair_returns_error(adapter).await;
    }

    fn test_venue_id_stable(adapter: &dyn LiquidityAdapter) {
        let id1 = adapter.venue_id().to_string();
        let id2 = adapter.venue_id().to_string();
        assert_eq!(id1, id2, "venue_id() must be stable and deterministic");
        assert!(!id1.is_empty(), "venue_id() must not be empty");
    }

    fn test_capabilities_consistent(adapter: &dyn LiquidityAdapter) {
        let caps = adapter.capabilities();
        assert_eq!(
            caps.venue_id,
            adapter.venue_id(),
            "capabilities().venue_id must match venue_id()"
        );
        assert!(
            !caps.display_name.is_empty(),
            "capabilities().display_name must not be empty"
        );
    }

    fn test_limits_positive(adapter: &dyn LiquidityAdapter) {
        let limits = adapter.limits();
        assert!(limits.price_precision > 0, "price_precision must be > 0");
        assert!(
            limits.quantity_precision > 0,
            "quantity_precision must be > 0"
        );
    }

    async fn test_health_returns(adapter: &dyn LiquidityAdapter) {
        // Must return without panic — any variant is acceptable in tests.
        let _h = adapter.health().await;
    }

    async fn test_unsupported_pair_returns_error(adapter: &dyn LiquidityAdapter) {
        // Using a nonsense asset pair: an obviously unsupported pair should
        // return an error rather than panic.
        let fake_sell = Asset::new("FAKE_SELL_XXXXXXXX", "GNONSENSE0000000000000000000000000000000000000000000");
        let fake_buy = Asset::new("FAKE_BUY_XXXXXXXXX", "GNONSENSE0000000000000000000000000000000000000000001");
        let amount = BigDecimal::from_str("1.0").unwrap();
        let result = adapter.get_quote(&fake_sell, &fake_buy, &amount).await;
        assert!(
            result.is_err(),
            "get_quote with a nonsense pair must return an error, not Ok"
        );
    }
}
