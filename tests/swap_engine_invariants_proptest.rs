//! Property-based tests for Swap Engine Invariants.
//!
//! Generates random sequences of quote requests, order placements, partial fills,
//! full fills, cancellations, and expirations, verifying that all swap invariants
//! hold at every single intermediate state.

use bigdecimal::{BigDecimal, Zero};
use chrono::{Duration, Utc};
use proptest::prelude::*;
use std::collections::HashMap;
use uuid::Uuid;

use swap_invariants::{
    MockLiquidityProvider, MockOracle, OrderSide, QuoteRequest, RealisticSwapEngine,
};

const ASSETS: &[&str] = &["USDC", "XLM", "BTC", "ETH"];

#[derive(Debug, Clone)]
enum SwapAction {
    Quote {
        source_idx: usize,
        target_idx: usize,
        amount_units: u64,
        max_slippage_bps: u32,
    },
    PlaceOrder {
        quote_slot: usize,
    },
    PartialFill {
        order_slot: usize,
        pct: u8,
    },
    FullFill {
        order_slot: usize,
    },
    Cancel {
        order_slot: usize,
    },
    Expire {
        order_slot: usize,
    },
}

fn action_strategy() -> impl Strategy<Value = SwapAction> {
    prop_oneof![
        3 => (
            0..ASSETS.len(),
            0..ASSETS.len(),
            1u64..10_000u64,
            0u32..500u32
        ).prop_map(|(s, t, amt, slip)| SwapAction::Quote {
            source_idx: s,
            target_idx: t,
            amount_units: amt,
            max_slippage_bps: slip,
        }),
        3 => (0usize..50).prop_map(|slot| SwapAction::PlaceOrder { quote_slot: slot }),
        3 => (0usize..50, 1u8..99u8).prop_map(|(slot, pct)| SwapAction::PartialFill {
            order_slot: slot,
            pct,
        }),
        2 => (0usize..50).prop_map(|slot| SwapAction::FullFill { order_slot: slot }),
        1 => (0usize..50).prop_map(|slot| SwapAction::Cancel { order_slot: slot }),
        1 => (0usize..50).prop_map(|slot| SwapAction::Expire { order_slot: slot }),
    ]
}

fn create_test_engine() -> RealisticSwapEngine {
    let mut initial_inventory = HashMap::new();
    initial_inventory.insert("USDC".to_string(), BigDecimal::from(10_000_000));
    initial_inventory.insert("XLM".to_string(), BigDecimal::from(50_000_000));
    initial_inventory.insert("BTC".to_string(), BigDecimal::from(1_000));
    initial_inventory.insert("ETH".to_string(), BigDecimal::from(20_000));

    let oracle = MockOracle::new();
    let lp = MockLiquidityProvider::new(30, 10);
    RealisticSwapEngine::new(initial_inventory, oracle, lp)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(50))]

    #[test]
    fn prop_swap_engine_invariants_hold_over_action_sequences(
        actions in prop::collection::vec(action_strategy(), 1..60)
    ) {
        let mut engine = create_test_engine();
        prop_assert!(engine.check_invariants().is_ok());

        let mut quote_ids: Vec<Uuid> = Vec::new();
        let mut order_ids: Vec<Uuid> = Vec::new();

        for action in actions {
            match action {
                SwapAction::Quote { source_idx, target_idx, amount_units, max_slippage_bps } => {
                    let source = ASSETS[source_idx % ASSETS.len()];
                    let target = ASSETS[target_idx % ASSETS.len()];
                    if source != target {
                        let req = QuoteRequest {
                            id: Uuid::new_v4(),
                            tenant_id: None,
                            source_asset: source.to_string(),
                            target_asset: target.to_string(),
                            amount: BigDecimal::from(amount_units),
                            side: OrderSide::Buy,
                            min_output_amount: None,
                            max_slippage_bps,
                            deadline: Utc::now() + Duration::minutes(10),
                        };
                        if let Ok(quote) = engine.request_quote(req) {
                            quote_ids.push(quote.id);
                        }
                    }
                }
                SwapAction::PlaceOrder { quote_slot } => {
                    if !quote_ids.is_empty() {
                        let quote_id = quote_ids[quote_slot % quote_ids.len()];
                        if let Ok(order_id) = engine.place_order(quote_id) {
                            order_ids.push(order_id);
                        }
                    }
                }
                SwapAction::PartialFill { order_slot, pct } => {
                    if !order_ids.is_empty() {
                        let order_id = order_ids[order_slot % order_ids.len()];
                        if let Some(order) = engine.state().orders.get(&order_id) {
                            if !order.status.is_terminal() {
                                let remaining = &order.requested_amount - &order.filled_amount;
                                let pct_decimal = BigDecimal::from(pct) / BigDecimal::from(100);
                                let fill_amount = (&remaining * &pct_decimal).round(6);
                                if fill_amount > BigDecimal::zero() && fill_amount <= remaining {
                                    let _ = engine.execute_fill(order_id, fill_amount);
                                }
                            }
                        }
                    }
                }
                SwapAction::FullFill { order_slot } => {
                    if !order_ids.is_empty() {
                        let order_id = order_ids[order_slot % order_ids.len()];
                        if let Some(order) = engine.state().orders.get(&order_id) {
                            if !order.status.is_terminal() {
                                let remaining = &order.requested_amount - &order.filled_amount;
                                if remaining > BigDecimal::zero() {
                                    let _ = engine.execute_fill(order_id, remaining);
                                }
                            }
                        }
                    }
                }
                SwapAction::Cancel { order_slot } => {
                    if !order_ids.is_empty() {
                        let order_id = order_ids[order_slot % order_ids.len()];
                        let _ = engine.cancel_order(order_id);
                    }
                }
                SwapAction::Expire { order_slot } => {
                    if !order_ids.is_empty() {
                        let order_id = order_ids[order_slot % order_ids.len()];
                        let _ = engine.expire_order(order_id);
                    }
                }
            }

            // Invariant assertion at each step
            let inv_result = engine.check_invariants();
            prop_assert!(
                inv_result.is_ok(),
                "Invariant violated during simulation: {:?}",
                inv_result.err()
            );
        }
    }

    #[test]
    fn prop_oracle_extreme_ratios_preserve_arithmetic_safety(
        input_amount in 1u64..1_000_000u64,
        spread_bps in 0u32..500u32,
        fee_bps in 0u32..200u32,
    ) {
        let engine = create_test_engine();
        let req = QuoteRequest {
            id: Uuid::new_v4(),
            tenant_id: None,
            source_asset: "USDC".to_string(),
            target_asset: "XLM".to_string(),
            amount: BigDecimal::from(input_amount),
            side: OrderSide::Buy,
            min_output_amount: None,
            max_slippage_bps: 100,
            deadline: Utc::now() + Duration::minutes(10),
        };

        let lp = MockLiquidityProvider::new(spread_bps, fee_bps);
        let oracle = MockOracle::new();
        if let Ok((output, fee, _)) = lp.quote(&oracle, &req) {
            prop_assert!(output > BigDecimal::zero());
            prop_assert!(fee >= BigDecimal::zero());
        }

        prop_assert!(engine.check_invariants().is_ok());
    }
}
