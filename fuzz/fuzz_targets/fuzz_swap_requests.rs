#![no_main]

use arbitrary::Arbitrary;
use bigdecimal::{BigDecimal, Zero};
use chrono::{Duration, Utc};
use libfuzzer_sys::fuzz_target;
use std::collections::HashMap;
use std::str::FromStr;
use uuid::Uuid;

use swap_invariants::{
    MockLiquidityProvider, MockOracle, OrderSide, QuoteRequest, RealisticSwapEngine,
};

#[derive(Debug, Arbitrary)]
struct FuzzSwapInput {
    source_asset_code: String,
    target_asset_code: String,
    amount_units: i64,
    amount_scale: i8,
    max_slippage_bps: u32,
    spread_bps: u32,
    fee_bps: u32,
    execute_order: bool,
    fill_fraction_pct: u8,
}

fuzz_target!(|input: FuzzSwapInput| {
    // Bound the scale to avoid unbounded allocations in BigDecimal
    let scale = (input.amount_scale as i64).clamp(-18, 18);
    let amount_str = format!("{}e{}", input.amount_units, scale);
    let amount = match BigDecimal::from_str(&amount_str) {
        Ok(a) => a,
        Err(_) => return,
    };

    let mut initial_inventory = HashMap::new();
    initial_inventory.insert("USDC".to_string(), BigDecimal::from(100_000));
    initial_inventory.insert("XLM".to_string(), BigDecimal::from(500_000));
    initial_inventory.insert("BTC".to_string(), BigDecimal::from(100));

    let oracle = MockOracle::new();
    let lp = MockLiquidityProvider::new(
        input.spread_bps.clamp(0, 10_000),
        input.fee_bps.clamp(0, 10_000),
    );
    let mut engine = RealisticSwapEngine::new(initial_inventory, oracle, lp);

    let req = QuoteRequest {
        id: Uuid::new_v4(),
        tenant_id: None,
        source_asset: input.source_asset_code,
        target_asset: input.target_asset_code,
        amount,
        side: OrderSide::Buy,
        min_output_amount: None,
        max_slippage_bps: input.max_slippage_bps.clamp(0, 10_000),
        deadline: Utc::now() + Duration::minutes(10),
    };

    if let Ok(quote) = engine.request_quote(req) {
        if input.execute_order {
            if let Ok(order_id) = engine.place_order(quote.id) {
                let pct = input.fill_fraction_pct.clamp(1, 100);
                if let Some(order) = engine.state().orders.get(&order_id) {
                    let fill_amt =
                        (&order.requested_amount * &BigDecimal::from(pct)) / BigDecimal::from(100);
                    if fill_amt > BigDecimal::zero() && fill_amt <= order.requested_amount {
                        let _ = engine.execute_fill(order_id, fill_amt);
                    }
                }
            }
        }
    }

    // Invariants must hold on every explored path
    assert!(
        engine.check_invariants().is_ok(),
        "Swap engine invariant violated during fuzzing"
    );
});
