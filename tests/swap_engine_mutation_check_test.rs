//! Mutation check unit tests confirming that deliberate invariant violations
//! are detected and categorized into exact `SwapInvariantViolation` variants.

use bigdecimal::{BigDecimal, Zero};
use chrono::Utc;
use std::collections::HashMap;
use uuid::Uuid;

use swap_invariants::{
    Fill, MockLiquidityProvider, MockOracle, RealisticSwapEngine, SwapInvariantViolation,
    SwapOrder, SwapOrderStatus,
};

fn create_valid_engine() -> RealisticSwapEngine {
    let mut initial_inventory = HashMap::new();
    initial_inventory.insert("USDC".to_string(), BigDecimal::from(100_000));
    initial_inventory.insert("XLM".to_string(), BigDecimal::from(500_000));

    let oracle = MockOracle::new();
    let lp = MockLiquidityProvider::new(30, 10);
    RealisticSwapEngine::new(initial_inventory, oracle, lp)
}

#[test]
fn test_valid_engine_has_no_invariant_violations() {
    let engine = create_valid_engine();
    assert!(engine.check_invariants().is_ok());
}

#[test]
fn test_mutation_reservation_exceeds_inventory() {
    let mut engine = create_valid_engine();

    // Mutate inventory: reserved > total
    if let Some(inv) = engine.state_mut().inventories.get_mut("USDC") {
        inv.reserved_amount = BigDecimal::from(150_000); // total is 100_000
    }

    let result = engine.check_invariants();
    assert!(result.is_err());
    match result.unwrap_err() {
        SwapInvariantViolation::ReservationExceedsInventory {
            asset,
            reserved,
            total,
            ..
        } => {
            assert_eq!(asset, "USDC");
            assert_eq!(reserved, BigDecimal::from(150_000));
            assert_eq!(total, BigDecimal::from(100_000));
        }
        other => panic!("Expected ReservationExceedsInventory, got {:?}", other),
    }
}

#[test]
fn test_mutation_fills_exceed_order_amount() {
    let mut engine = create_valid_engine();
    let order_id = Uuid::new_v4();

    // Insert order with filled_amount > requested_amount
    let order = SwapOrder {
        id: order_id,
        quote_id: Uuid::new_v4(),
        tenant_id: None,
        status: SwapOrderStatus::PartiallyFilled,
        source_asset: "USDC".to_string(),
        target_asset: "XLM".to_string(),
        requested_amount: BigDecimal::from(100),
        filled_amount: BigDecimal::from(150), // exceeds 100!
        reserved_amount: BigDecimal::from(0),
        fills: vec![Fill {
            fill_id: Uuid::new_v4(),
            order_id,
            amount_in: BigDecimal::from(150),
            amount_out: BigDecimal::from(1200),
            fee_paid: BigDecimal::from(1),
            executed_at: Utc::now(),
        }],
        created_at: Utc::now(),
        updated_at: Utc::now(),
        expires_at: Utc::now(),
    };

    engine.state_mut().orders.insert(order_id, order);

    let result = engine.check_invariants();
    assert!(result.is_err());
    match result.unwrap_err() {
        SwapInvariantViolation::FillsExceedOrderAmount {
            order_id: id,
            filled,
            requested,
        } => {
            assert_eq!(id, order_id);
            assert_eq!(filled, BigDecimal::from(150));
            assert_eq!(requested, BigDecimal::from(100));
        }
        other => panic!("Expected FillsExceedOrderAmount, got {:?}", other),
    }
}

#[test]
fn test_mutation_terminal_order_holds_reservation() {
    let mut engine = create_valid_engine();
    let order_id = Uuid::new_v4();

    // Order is cancelled, but retains reserved_amount > 0
    let order = SwapOrder {
        id: order_id,
        quote_id: Uuid::new_v4(),
        tenant_id: None,
        status: SwapOrderStatus::Cancelled,
        source_asset: "USDC".to_string(),
        target_asset: "XLM".to_string(),
        requested_amount: BigDecimal::from(100),
        filled_amount: BigDecimal::zero(),
        reserved_amount: BigDecimal::from(50), // should be 0 on terminal!
        fills: Vec::new(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        expires_at: Utc::now(),
    };

    engine.state_mut().orders.insert(order_id, order);

    let result = engine.check_invariants();
    assert!(result.is_err());
    match result.unwrap_err() {
        SwapInvariantViolation::TerminalOrderTransition {
            order_id: id,
            terminal_status,
            ..
        } => {
            assert_eq!(id, order_id);
            assert_eq!(terminal_status, SwapOrderStatus::Cancelled);
        }
        other => panic!("Expected TerminalOrderTransition, got {:?}", other),
    }
}

#[test]
fn test_mutation_illegal_state_transition() {
    let mut engine = create_valid_engine();
    let order_id = Uuid::new_v4();

    // Status is Filled, but filled_amount < requested_amount
    let order = SwapOrder {
        id: order_id,
        quote_id: Uuid::new_v4(),
        tenant_id: None,
        status: SwapOrderStatus::Filled,
        source_asset: "USDC".to_string(),
        target_asset: "XLM".to_string(),
        requested_amount: BigDecimal::from(100),
        filled_amount: BigDecimal::from(50), // Only half filled!
        reserved_amount: BigDecimal::zero(),
        fills: vec![Fill {
            fill_id: Uuid::new_v4(),
            order_id,
            amount_in: BigDecimal::from(50),
            amount_out: BigDecimal::from(400),
            fee_paid: BigDecimal::from(1),
            executed_at: Utc::now(),
        }],
        created_at: Utc::now(),
        updated_at: Utc::now(),
        expires_at: Utc::now(),
    };

    engine.state_mut().orders.insert(order_id, order);

    let result = engine.check_invariants();
    assert!(result.is_err());
    match result.unwrap_err() {
        SwapInvariantViolation::IllegalStateTransition {
            order_id: id,
            from,
            to,
        } => {
            assert_eq!(id, order_id);
            assert_eq!(from, SwapOrderStatus::PartiallyFilled);
            assert_eq!(to, SwapOrderStatus::Filled);
        }
        other => panic!("Expected IllegalStateTransition, got {:?}", other),
    }
}

#[test]
fn test_mutation_value_conservation_violated() {
    let mut engine = create_valid_engine();
    let order_id = Uuid::new_v4();

    // Fill has non-positive amount_out
    let order = SwapOrder {
        id: order_id,
        quote_id: Uuid::new_v4(),
        tenant_id: None,
        status: SwapOrderStatus::PartiallyFilled,
        source_asset: "USDC".to_string(),
        target_asset: "XLM".to_string(),
        requested_amount: BigDecimal::from(100),
        filled_amount: BigDecimal::from(50),
        reserved_amount: BigDecimal::from(50),
        fills: vec![Fill {
            fill_id: Uuid::new_v4(),
            order_id,
            amount_in: BigDecimal::from(50),
            amount_out: BigDecimal::zero(), // 0 output for positive input!
            fee_paid: BigDecimal::from(1),
            executed_at: Utc::now(),
        }],
        created_at: Utc::now(),
        updated_at: Utc::now(),
        expires_at: Utc::now(),
    };

    engine.state_mut().orders.insert(order_id, order);

    let result = engine.check_invariants();
    assert!(result.is_err());
    match result.unwrap_err() {
        SwapInvariantViolation::ValueConservationViolated { order_id: id, .. } => {
            assert_eq!(id, order_id);
        }
        other => panic!("Expected ValueConservationViolated, got {:?}", other),
    }
}

#[test]
fn test_mutation_arithmetic_safety_negative_inventory() {
    let mut engine = create_valid_engine();

    // Negative total inventory
    if let Some(inv) = engine.state_mut().inventories.get_mut("USDC") {
        inv.total_amount = BigDecimal::from(-10);
    }

    let result = engine.check_invariants();
    assert!(result.is_err());
    match result.unwrap_err() {
        SwapInvariantViolation::ArithmeticSafetyViolated { reason } => {
            assert!(reason.contains("Negative total inventory"));
        }
        other => panic!("Expected ArithmeticSafetyViolated, got {:?}", other),
    }
}
