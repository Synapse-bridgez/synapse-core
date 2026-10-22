//! Swap order execution engine.
//!
//! Routes orders to external venues and, when enabled, crosses offsetting
//! tenant flows internally before falling back to external execution.

pub mod crossing;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

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
