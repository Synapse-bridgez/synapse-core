//! Realistic Swap Engine implementation with MockOracle and MockLiquidityProvider.
//!
//! Maintains inventory balance, manages order lifecycle state machines,
//! executes partial/full fills with value conservation, and checks domain invariants.

use bigdecimal::{BigDecimal, One, Zero};
use chrono::{Duration, Utc};
use std::collections::HashMap;
use std::str::FromStr;
use uuid::Uuid;

use super::invariants::{
    check_invariants, Fill, Inventory, Quote, QuoteRequest, SwapEngineState,
    SwapInvariantViolation, SwapOrder, SwapOrderStatus,
};
use super::SwapError;

/// Simulated price oracle providing exchange rates across asset pairs.
#[derive(Debug, Clone)]
pub struct MockOracle {
    prices: HashMap<(String, String), BigDecimal>,
}

impl Default for MockOracle {
    fn default() -> Self {
        Self::new()
    }
}

impl MockOracle {
    pub fn new() -> Self {
        let mut prices = HashMap::new();
        // 1 USDC = 8.0 XLM, 1 XLM = 0.125 USDC
        prices.insert(
            ("USDC".to_string(), "XLM".to_string()),
            BigDecimal::from_str("8.0").unwrap(),
        );
        prices.insert(
            ("XLM".to_string(), "USDC".to_string()),
            BigDecimal::from_str("0.125").unwrap(),
        );
        // 1 BTC = 60,000 USDC, 1 USDC = 0.00001666 BTC
        prices.insert(
            ("BTC".to_string(), "USDC".to_string()),
            BigDecimal::from_str("60000.0").unwrap(),
        );
        prices.insert(
            ("USDC".to_string(), "BTC".to_string()),
            BigDecimal::from_str("0.00001666").unwrap(),
        );
        // 1 ETH = 3,000 USDC, 1 USDC = 0.00033333 ETH
        prices.insert(
            ("ETH".to_string(), "USDC".to_string()),
            BigDecimal::from_str("3000.0").unwrap(),
        );
        prices.insert(
            ("USDC".to_string(), "ETH".to_string()),
            BigDecimal::from_str("0.00033333").unwrap(),
        );

        Self { prices }
    }

    pub fn set_price(&mut self, base: &str, quote: &str, price: BigDecimal) {
        self.prices
            .insert((base.to_string(), quote.to_string()), price);
    }

    pub fn get_price(&self, base: &str, quote: &str) -> Option<BigDecimal> {
        if base == quote {
            return Some(BigDecimal::one());
        }
        self.prices
            .get(&(base.to_string(), quote.to_string()))
            .cloned()
    }
}

/// Simulated liquidity provider for fee and spread calculation.
#[derive(Debug, Clone)]
pub struct MockLiquidityProvider {
    pub spread_bps: u32,
    pub fee_bps: u32,
}

impl Default for MockLiquidityProvider {
    fn default() -> Self {
        Self {
            spread_bps: 30, // 0.30%
            fee_bps: 10,    // 0.10%
        }
    }
}

impl MockLiquidityProvider {
    pub fn new(spread_bps: u32, fee_bps: u32) -> Self {
        Self {
            spread_bps,
            fee_bps,
        }
    }

    pub fn quote(
        &self,
        oracle: &MockOracle,
        req: &QuoteRequest,
    ) -> Result<(BigDecimal, BigDecimal, u32), SwapError> {
        let price = oracle
            .get_price(&req.source_asset, &req.target_asset)
            .ok_or_else(|| {
                SwapError::InvalidHandoff(format!(
                    "No price route available for {} -> {}",
                    req.source_asset, req.target_asset
                ))
            })?;

        // fee = amount * fee_bps / 10000
        let bps_divisor = BigDecimal::from(10_000);
        let fee_factor = BigDecimal::from(self.fee_bps) / &bps_divisor;
        let fee_amount = &req.amount * &fee_factor;

        let net_amount = &req.amount - &fee_amount;
        if net_amount <= BigDecimal::zero() {
            return Err(SwapError::InvalidHandoff(
                "Fee exceeds swap amount".to_string(),
            ));
        }

        // spread discount = 1 - (spread_bps / 10000)
        let spread_factor = BigDecimal::from(self.spread_bps) / &bps_divisor;
        let effective_rate = &price * (BigDecimal::one() - spread_factor);
        let output_amount = net_amount * effective_rate;

        if let Some(min_out) = &req.min_output_amount {
            if &output_amount < min_out {
                return Err(SwapError::InvalidHandoff(format!(
                    "Quote output {} below required min_output_amount {}",
                    output_amount, min_out
                )));
            }
        }

        Ok((output_amount, fee_amount, self.spread_bps))
    }
}

/// Realistic in-memory swap engine managing the full quote-reserve-fill-cancel lifecycle.
#[derive(Debug, Clone)]
pub struct RealisticSwapEngine {
    state: SwapEngineState,
    oracle: MockOracle,
    lp: MockLiquidityProvider,
    quotes: HashMap<Uuid, Quote>,
}

impl RealisticSwapEngine {
    /// Initialize with inventory per asset and components.
    pub fn new(
        initial_inventory: HashMap<String, BigDecimal>,
        oracle: MockOracle,
        lp: MockLiquidityProvider,
    ) -> Self {
        let mut inventories = HashMap::new();
        for (asset, total) in initial_inventory {
            inventories.insert(asset.clone(), Inventory::new(asset, total));
        }

        Self {
            state: SwapEngineState {
                inventories,
                orders: HashMap::new(),
            },
            oracle,
            lp,
            quotes: HashMap::new(),
        }
    }

    /// Access engine state.
    pub fn state(&self) -> &SwapEngineState {
        &self.state
    }

    /// Mutable access to engine state (for testing / injection).
    pub fn state_mut(&mut self) -> &mut SwapEngineState {
        &mut self.state
    }

    /// Check all swap engine invariants.
    pub fn check_invariants(&self) -> Result<(), SwapInvariantViolation> {
        check_invariants(&self.state)
    }

    /// Request a new quote.
    pub fn request_quote(&mut self, req: QuoteRequest) -> Result<Quote, SwapError> {
        if req.amount <= BigDecimal::zero() {
            return Err(SwapError::InvalidHandoff(
                "Swap amount must be positive".to_string(),
            ));
        }

        let (output_amount, fee_amount, spread_bps) = self.lp.quote(&self.oracle, &req)?;
        let quote = Quote {
            id: Uuid::new_v4(),
            request_id: req.id,
            source_asset: req.source_asset,
            target_asset: req.target_asset,
            input_amount: req.amount,
            output_amount,
            fee_amount,
            spread_bps,
            expires_at: Utc::now() + Duration::minutes(5),
            signature: format!("sig:{}", Uuid::new_v4()),
        };

        self.quotes.insert(quote.id, quote.clone());
        Ok(quote)
    }

    /// Place an order from an existing quote and reserve inventory.
    pub fn place_order(&mut self, quote_id: Uuid) -> Result<Uuid, SwapError> {
        let quote = self
            .quotes
            .get(&quote_id)
            .ok_or_else(|| SwapError::InvalidHandoff("Quote not found".to_string()))?
            .clone();

        if quote.expires_at < Utc::now() {
            return Err(SwapError::InvalidHandoff("Quote has expired".to_string()));
        }

        let inv = self
            .state
            .inventories
            .get_mut(&quote.source_asset)
            .ok_or_else(|| {
                SwapError::InvalidHandoff(format!(
                    "Asset {} not supported in inventory",
                    quote.source_asset
                ))
            })?;

        if inv.available_amount() < quote.input_amount {
            return Err(SwapError::Unavailable);
        }

        // Reserve inventory
        inv.reserved_amount = &inv.reserved_amount + &quote.input_amount;

        let now = Utc::now();
        let order_id = Uuid::new_v4();
        let order = SwapOrder {
            id: order_id,
            quote_id,
            tenant_id: None,
            status: SwapOrderStatus::Reserved,
            source_asset: quote.source_asset.clone(),
            target_asset: quote.target_asset.clone(),
            requested_amount: quote.input_amount.clone(),
            filled_amount: BigDecimal::zero(),
            reserved_amount: quote.input_amount,
            fills: Vec::new(),
            created_at: now,
            updated_at: now,
            expires_at: quote.expires_at,
        };

        self.state.orders.insert(order_id, order);

        self.check_invariants().map_err(|e| {
            SwapError::Internal(format!("Invariant violated after placing order: {e}"))
        })?;

        Ok(order_id)
    }

    /// Execute a fill (full or partial) on an active order.
    pub fn execute_fill(
        &mut self,
        order_id: Uuid,
        fill_amount: BigDecimal,
    ) -> Result<Fill, SwapError> {
        if fill_amount <= BigDecimal::zero() {
            return Err(SwapError::InvalidHandoff(
                "Fill amount must be positive".to_string(),
            ));
        }

        let order = self
            .state
            .orders
            .get_mut(&order_id)
            .ok_or_else(|| SwapError::InvalidHandoff("Order not found".to_string()))?;

        if order.status.is_terminal() {
            return Err(SwapError::InvalidHandoff(format!(
                "Cannot fill order in terminal state {:?}",
                order.status
            )));
        }

        let remaining = &order.requested_amount - &order.filled_amount;
        if fill_amount > remaining {
            return Err(SwapError::InvalidHandoff(format!(
                "Fill amount {} exceeds remaining order amount {}",
                fill_amount, remaining
            )));
        }

        let quote = self
            .quotes
            .get(&order.quote_id)
            .ok_or_else(|| SwapError::Internal("Associated quote not found".to_string()))?
            .clone();

        // Calculate proportional output and fee: fill_ratio = fill_amount / quote.input_amount
        let fill_ratio = &fill_amount / &quote.input_amount;
        let amount_out = &quote.output_amount * &fill_ratio;
        let fee_paid = &quote.fee_amount * &fill_ratio;

        let now = Utc::now();
        let fill = Fill {
            fill_id: Uuid::new_v4(),
            order_id,
            amount_in: fill_amount.clone(),
            amount_out,
            fee_paid,
            executed_at: now,
        };

        order.fills.push(fill.clone());
        order.filled_amount = &order.filled_amount + &fill_amount;
        order.reserved_amount = &order.reserved_amount - &fill_amount;
        order.updated_at = now;

        // Inventory total and reserved are reduced by the filled amount
        if let Some(inv) = self.state.inventories.get_mut(&order.source_asset) {
            inv.reserved_amount = &inv.reserved_amount - &fill_amount;
            inv.total_amount = &inv.total_amount - &fill_amount;
        }

        if order.filled_amount == order.requested_amount {
            order.status = SwapOrderStatus::Filled;
            order.reserved_amount = BigDecimal::zero();
        } else {
            order.status = SwapOrderStatus::PartiallyFilled;
        }

        self.check_invariants()
            .map_err(|e| SwapError::Internal(format!("Invariant violated after fill: {e}")))?;

        Ok(fill)
    }

    /// Cancel an active order and release its reserved inventory.
    pub fn cancel_order(&mut self, order_id: Uuid) -> Result<(), SwapError> {
        let order = self
            .state
            .orders
            .get_mut(&order_id)
            .ok_or_else(|| SwapError::InvalidHandoff("Order not found".to_string()))?;

        if order.status.is_terminal() {
            return Err(SwapError::InvalidHandoff(format!(
                "Cannot cancel order in terminal state {:?}",
                order.status
            )));
        }

        let release_amount = order.reserved_amount.clone();
        order.status = SwapOrderStatus::Cancelled;
        order.reserved_amount = BigDecimal::zero();
        order.updated_at = Utc::now();

        if let Some(inv) = self.state.inventories.get_mut(&order.source_asset) {
            inv.reserved_amount = &inv.reserved_amount - &release_amount;
        }

        self.check_invariants()
            .map_err(|e| SwapError::Internal(format!("Invariant violated after cancel: {e}")))?;

        Ok(())
    }

    /// Expire an active order and release its reserved inventory.
    pub fn expire_order(&mut self, order_id: Uuid) -> Result<(), SwapError> {
        let order = self
            .state
            .orders
            .get_mut(&order_id)
            .ok_or_else(|| SwapError::InvalidHandoff("Order not found".to_string()))?;

        if order.status.is_terminal() {
            return Err(SwapError::InvalidHandoff(format!(
                "Cannot expire order in terminal state {:?}",
                order.status
            )));
        }

        let release_amount = order.reserved_amount.clone();
        order.status = SwapOrderStatus::Expired;
        order.reserved_amount = BigDecimal::zero();
        order.updated_at = Utc::now();

        if let Some(inv) = self.state.inventories.get_mut(&order.source_asset) {
            inv.reserved_amount = &inv.reserved_amount - &release_amount;
        }

        self.check_invariants()
            .map_err(|e| SwapError::Internal(format!("Invariant violated after expire: {e}")))?;

        Ok(())
    }
}
