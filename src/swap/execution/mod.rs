//! Swap order execution engine.
//!
//! Provides the execution-algorithm layer that slices a large parent swap
//! order into child orders scheduled over a configurable window. Child orders
//! are submitted through the same order submission path as normal orders so
//! idempotency, risk/compliance checks, and slippage guards all apply.
//!
//! Routes orders to external venues and, when enabled, crosses offsetting
//! tenant flows internally before falling back to external execution.
//!
//! Also wires in the balance reservation ledger that prevents concurrent
//! in-flight orders from over-committing the same funds.

pub mod crossing;
pub mod reservation;

pub use reservation::{
    AvailableBalance, ReservationError, ReservationLedger, ReservationRecord, ReservationState,
};

use std::collections::VecDeque;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Terminal behavior for the unfilled remainder when the execution window
/// expires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalBehavior {
    /// Fire the remaining quantity as a single market order.
    Market,
    /// Cancel the remainder and leave the parent partially filled.
    Cancel,
    /// Extend the window and keep working the remainder.
    Extend,
}

/// Urgency influences slice sizing (higher urgency front-loads volume).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Urgency {
    Low,
    Normal,
    High,
}

impl Urgency {
    /// Multiplier applied to the base slice size.
    fn multiplier(self) -> f64 {
        match self {
            Urgency::Low => 0.75,
            Urgency::Normal => 1.0,
            Urgency::High => 1.5,
        }
    }
}

/// Side of an order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderSide {
    Buy,
    Sell,
}

/// Venue attribution for a fill. Internal fills are crossed between tenants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FillVenue {
    External(String),
    Internal(String),
}

/// A fill record. Internal and external fills share the same shape so that
/// downstream settlement and reconciliation need no special cases.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fill {
    pub order_id: String,
    pub asset_pair: String,
    pub side: OrderSide,
    pub quantity: Decimal,
    pub price: Decimal,
    pub venue: FillVenue,
}

/// Errors raised during execution.
#[derive(Debug)]
pub enum ExecutionError {
    /// A proposed internal cross was abandoned; caller should fall back to
    /// external execution.
    CrossAbandoned(crossing::CrossAudit),
    /// External venue execution failed.
    Venue(String),
}

impl std::fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecutionError::CrossAbandoned(audit) => write!(
                f,
                "internal cross abandoned: {}",
                audit.reason.as_deref().unwrap_or("unknown")
            ),
            ExecutionError::Venue(msg) => write!(f, "venue execution failed: {msg}"),
        }
    }
}

impl std::error::Error for ExecutionError {}

/// Result alias for execution operations.
pub type ExecutionResult<T> = Result<T, ExecutionError>;
