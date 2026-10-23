//! Core types for the swap quote/order domain.
//!
//! Amounts are exchanged as [`BigDecimal`] at the API boundary but are
//! quantized to [`AMOUNT_SCALE`] decimal places (Stellar's precision) before
//! any allocation or fill arithmetic happens. This keeps split allocation
//! exact: the sum of every leg always equals the requested amount, with no
//! floating point drift.

use bigdecimal::BigDecimal;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;
use uuid::Uuid;

/// Decimal places used for every swap amount. Stellar assets use 7dp.
pub const AMOUNT_SCALE: i64 = 7;

/// One unit of the smallest representable amount (10^-7).
pub const UNIT_SCALE_FACTOR: i64 = 10_000_000;

/// Where a leg of a swap is expected to execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiquiditySourceKind {
    /// Stellar's built-in decentralized exchange (path payment / order book).
    StellarDex,
    /// An Anchor Platform venue (off-chain settlement anchored on Stellar).
    AnchorPlatform,
    /// The platform's own internal order book.
    InternalBook,
}

impl LiquiditySourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LiquiditySourceKind::StellarDex => "stellar_dex",
            LiquiditySourceKind::AnchorPlatform => "anchor_platform",
            LiquiditySourceKind::InternalBook => "internal_book",
        }
    }
}

impl fmt::Display for LiquiditySourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for LiquiditySourceKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "stellar_dex" | "stellar-dex" | "stellar" => Ok(LiquiditySourceKind::StellarDex),
            "anchor_platform" | "anchor-platform" | "anchor" => {
                Ok(LiquiditySourceKind::AnchorPlatform)
            }
            "internal_book" | "internal-book" | "internal" => Ok(LiquiditySourceKind::InternalBook),
            other => Err(format!("unknown liquidity source kind: {other}")),
        }
    }
}

/// A candidate venue the router may split an order across.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiquiditySource {
    /// Stable identifier unique within one quote request (e.g. a pool id or
    /// Anchor Platform account). Used as the deterministic allocation
    /// tie-breaker when priorities and fractional remainders are equal.
    pub id: String,
    pub kind: LiquiditySourceKind,
    /// Buy-asset units received per one sell-asset unit, before fees.
    pub price: BigDecimal,
    /// Maximum sell-asset amount this source can absorb. Capped further by
    /// `max_leg_amount` when present.
    pub available: BigDecimal,
    /// Relative allocation weight. Sources with weight 0 are ignored.
    #[serde(default = "default_weight")]
    pub weight: u32,
    /// Lower priority numbers are allocated first and win deterministic
    /// tie-breaks. Defaults to `0`; when every source shares a priority the
    /// source `id` (lexicographic) is the tie-break.
    #[serde(default)]
    pub priority: u32,
    /// A leg smaller than this is not viable: the router removes the source
    /// and redistributes its allocation instead of producing a dust leg.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_leg_amount: Option<BigDecimal>,
    /// Optional hard cap on this source's leg, applied after `available`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_leg_amount: Option<BigDecimal>,
    /// Source fee in basis points, deducted from the buy amount of each leg.
    #[serde(default)]
    pub fee_bps: u32,
}

fn default_weight() -> u32 {
    1
}

/// A quoted leg, i.e. how a quote intends to split the sell amount.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuoteLeg {
    pub id: Uuid,
    pub source_id: String,
    pub source_kind: LiquiditySourceKind,
    pub priority: u32,
    /// Sell-asset amount allocated to this leg. Quote legs sum exactly to
    /// [`SwapQuote::sell_amount`].
    pub sell_amount: BigDecimal,
    /// Quoted price for this leg, before fees.
    pub price: BigDecimal,
    /// Expected buy amount net of [`Self::fee_bps`].
    pub expected_buy_amount: BigDecimal,
    pub fee_bps: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuoteStatus {
    Active,
    Consumed,
    Expired,
}

impl QuoteStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            QuoteStatus::Active => "active",
            QuoteStatus::Consumed => "consumed",
            QuoteStatus::Expired => "expired",
        }
    }
}

impl fmt::Display for QuoteStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A point-in-time swap price. Quotes expire automatically; an order can only
/// be created from an `Active` quote before `expires_at`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwapQuote {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub sell_asset: String,
    pub buy_asset: String,
    pub sell_amount: BigDecimal,
    pub estimated_buy_amount: BigDecimal,
    pub average_price: BigDecimal,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_price: Option<BigDecimal>,
    pub legs: Vec<QuoteLeg>,
    pub status: QuoteStatus,
    pub reservation: Reservation,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released_at: Option<DateTime<Utc>>,
}

impl SwapQuote {
    pub fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }
}

/// A reservation of client funds and limit budget while a quote/order is live.
///
/// `consumed_*` only ever counts value that was *actually* used. The amount
/// released on cancellation/expiry/timeout is therefore
/// `reserved - consumed`, computed once and recorded in a [`ReleaseRecord`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reservation {
    pub reserved_amount: BigDecimal,
    pub consumed_amount: BigDecimal,
    pub reserved_limit_budget: u32,
    pub consumed_limit_budget: u32,
}

impl Reservation {
    pub fn new(reserved_amount: BigDecimal, reserved_limit_budget: u32) -> Self {
        Self {
            reserved_amount,
            consumed_amount: BigDecimal::from(0),
            reserved_limit_budget,
            consumed_limit_budget: 0,
        }
    }

    /// Value still held but not yet used. Never negative.
    pub fn releasable_amount(&self) -> BigDecimal {
        let remaining = &self.reserved_amount - &self.consumed_amount;
        if remaining < BigDecimal::from(0) {
            BigDecimal::from(0)
        } else {
            remaining
        }
    }

    pub fn releasable_limit_budget(&self) -> u32 {
        self.reserved_limit_budget
            .saturating_sub(self.consumed_limit_budget)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    /// Accepted, no fill reported yet.
    Pending,
    /// Execution was explicitly started (`POST /orders/:id/start`), no fill yet.
    Executing,
    /// At least one leg has a partial fill.
    PartiallyFilled,
    /// Every leg is fully filled.
    Filled,
    /// Cancelled by the client; remaining reservation released.
    Cancelled,
    /// The backing quote expired before the order was created.
    Expired,
    /// The execution deadline passed before completion; reservation released.
    TimedOut,
    /// Execution failed terminally; reservation released.
    Failed,
}

impl OrderStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            OrderStatus::Pending => "pending",
            OrderStatus::Executing => "executing",
            OrderStatus::PartiallyFilled => "partially_filled",
            OrderStatus::Filled => "filled",
            OrderStatus::Cancelled => "cancelled",
            OrderStatus::Expired => "expired",
            OrderStatus::TimedOut => "timed_out",
            OrderStatus::Failed => "failed",
        }
    }

    /// Terminal states never transition again; a request that arrives after
    /// one of these must return the stored state rather than mutate it.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            OrderStatus::Filled
                | OrderStatus::Cancelled
                | OrderStatus::Expired
                | OrderStatus::TimedOut
                | OrderStatus::Failed
        )
    }

    /// Whether an order in this state can still be cancelled by a client.
    pub fn is_cancellable(self) -> bool {
        matches!(
            self,
            OrderStatus::Pending | OrderStatus::Executing | OrderStatus::PartiallyFilled
        )
    }
}

impl fmt::Display for OrderStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for OrderStatus {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "pending" => Ok(OrderStatus::Pending),
            "executing" => Ok(OrderStatus::Executing),
            "partially_filled" => Ok(OrderStatus::PartiallyFilled),
            "filled" => Ok(OrderStatus::Filled),
            "cancelled" | "canceled" => Ok(OrderStatus::Cancelled),
            "expired" => Ok(OrderStatus::Expired),
            "timed_out" | "timedout" => Ok(OrderStatus::TimedOut),
            "failed" => Ok(OrderStatus::Failed),
            other => Err(format!("unknown swap order status: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegStatus {
    Pending,
    PartiallyFilled,
    Filled,
    Released,
}

impl LegStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            LegStatus::Pending => "pending",
            LegStatus::PartiallyFilled => "partially_filled",
            LegStatus::Filled => "filled",
            LegStatus::Released => "released",
        }
    }
}

impl fmt::Display for LegStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Execution state for one source allocation. `filled_*` always reports what
/// actually executed, never the quote estimate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderLeg {
    pub id: Uuid,
    /// Quote leg this execution leg was created from, when the order came
    /// from a quote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote_leg_id: Option<Uuid>,
    pub source_id: String,
    pub source_kind: LiquiditySourceKind,
    pub priority: u32,
    pub allocated_sell_amount: BigDecimal,
    pub filled_sell_amount: BigDecimal,
    pub filled_buy_amount: BigDecimal,
    pub average_fill_price: BigDecimal,
    pub quoted_price: BigDecimal,
    pub fee_bps: u32,
    pub fill_count: u32,
    pub status: LegStatus,
}

impl OrderLeg {
    pub fn remaining_sell_amount(&self) -> BigDecimal {
        let remaining = &self.allocated_sell_amount - &self.filled_sell_amount;
        if remaining < BigDecimal::from(0) {
            BigDecimal::from(0)
        } else {
            remaining
        }
    }
}

/// One execution report from a venue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fill {
    pub id: Uuid,
    pub order_id: Uuid,
    pub leg_id: Uuid,
    pub source_id: String,
    pub sell_amount: BigDecimal,
    pub buy_amount: BigDecimal,
    /// Actual execution price (buy per sell), before the leg fee.
    pub price: BigDecimal,
    /// Venue-assigned reference (transaction hash, Anchor Platform id, …).
    /// Used for deduplication and reconciliation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub venue_reference: Option<String>,
    pub filled_at: DateTime<Utc>,
}

/// Why a reservation was released. Every terminal non-success path records
/// exactly one reason; successful fills keep the consumed budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseReason {
    ClientCancelled,
    QuoteExpired,
    ExecutionTimedOut,
    OrderFailed,
}

impl ReleaseReason {
    pub fn as_str(self) -> &'static str {
        match self {
            ReleaseReason::ClientCancelled => "client_cancelled",
            ReleaseReason::QuoteExpired => "quote_expired",
            ReleaseReason::ExecutionTimedOut => "execution_timed_out",
            ReleaseReason::OrderFailed => "order_failed",
        }
    }
}

impl fmt::Display for ReleaseReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Auditable record of a deterministic release. Created at most once per
/// quote/order; its presence is the idempotency marker for cancellation and
/// timeout handling.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReleaseRecord {
    pub reason: ReleaseReason,
    /// Unused sell-asset amount returned to the client.
    pub amount: BigDecimal,
    /// Unused limit budget returned to the tenant.
    pub limit_budget: u32,
    pub released_at: DateTime<Utc>,
}

/// Per-leg reporting of what actually filled. Returned by the order endpoint
/// as part of [`SwapOrder`]; kept as a separate type so reconciliation and
/// operator tooling can consume a compact view without parsing fill rows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LegReport {
    pub leg_id: Uuid,
    pub source_id: String,
    pub source_kind: LiquiditySourceKind,
    pub allocated_sell_amount: BigDecimal,
    pub filled_sell_amount: BigDecimal,
    pub remaining_sell_amount: BigDecimal,
    pub filled_buy_amount: BigDecimal,
    pub average_fill_price: BigDecimal,
    pub fill_count: u32,
    pub status: LegStatus,
}

/// A live swap order. The order keeps the exact allocation snapshot from its
/// quote, so delayed fills are still attributed to the leg that reserved the
/// liquidity even if another quote would route differently now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwapOrder {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub quote_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_order_id: Option<String>,
    pub sell_asset: String,
    pub buy_asset: String,
    pub sell_amount: BigDecimal,
    pub estimated_buy_amount: BigDecimal,
    pub filled_sell_amount: BigDecimal,
    pub filled_buy_amount: BigDecimal,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit_price: Option<BigDecimal>,
    pub status: OrderStatus,
    pub legs: Vec<OrderLeg>,
    pub fills: Vec<Fill>,
    pub reservation: Reservation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<ReleaseRecord>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub execution_deadline: DateTime<Utc>,
}

impl SwapOrder {
    pub fn is_terminal(&self) -> bool {
        self.status.is_terminal()
    }

    pub fn leg_reports(&self) -> Vec<LegReport> {
        self.legs
            .iter()
            .map(|leg| LegReport {
                leg_id: leg.id,
                source_id: leg.source_id.clone(),
                source_kind: leg.source_kind,
                allocated_sell_amount: leg.allocated_sell_amount.clone(),
                filled_sell_amount: leg.filled_sell_amount.clone(),
                remaining_sell_amount: leg.remaining_sell_amount(),
                filled_buy_amount: leg.filled_buy_amount.clone(),
                average_fill_price: leg.average_fill_price.clone(),
                fill_count: leg.fill_count,
                status: leg.status,
            })
            .collect()
    }
}

/// Request body for creating a quote.
#[derive(Debug, Clone, Deserialize)]
pub struct SwapQuoteRequest {
    pub sell_asset: String,
    pub buy_asset: String,
    pub sell_amount: BigDecimal,
    #[serde(default)]
    pub limit_price: Option<BigDecimal>,
    /// Optional quote lifetime. Clamped to the service's configured maximum.
    #[serde(default)]
    pub ttl_seconds: Option<u64>,
    /// Candidate liquidity sources to split the order across.
    pub sources: Vec<LiquiditySource>,
}

/// Errors returned by the swap service. Kept separate from [`crate::swap::SwapError`],
/// which is the Phase 2 engine hand-off error type.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum SwapOrderError {
    #[error("validation error: {0}")]
    Validation(String),

    #[error("swap quote or order not found")]
    NotFound,

    #[error("quote has expired")]
    QuoteExpired,

    #[error("quote has already been consumed by an order")]
    QuoteConsumed,

    #[error("insufficient liquidity: requested {requested}, available {available}")]
    InsufficientLiquidity {
        requested: BigDecimal,
        available: BigDecimal,
    },

    #[error("fill of {requested} exceeds remaining {remaining} on leg {leg_id}")]
    Overfill {
        leg_id: Uuid,
        requested: BigDecimal,
        remaining: BigDecimal,
    },

    #[error("duplicate fill for venue reference {0}")]
    DuplicateFill(String),

    #[error("invalid order state transition: {from} -> {to}")]
    InvalidState {
        from: OrderStatus,
        to: OrderStatus,
    },

    #[error("order execution deadline has passed")]
    ExecutionTimedOut,

    #[error("tenant limit budget exhausted")]
    LimitBudgetExhausted,

    #[error("internal swap state error: {0}")]
    Internal(String),

    #[error("order is not in a cancellable state: {0}")]
    NotCancellable(OrderStatus),
}

pub type SwapOrderResult<T> = Result<T, SwapOrderError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amount_scale_matches_stellar_precision() {
        assert_eq!(UNIT_SCALE_FACTOR, 10_i64.pow(AMOUNT_SCALE as u32));
    }

    #[test]
    fn order_status_terminality_and_cancellability() {
        assert!(OrderStatus::Filled.is_terminal());
        assert!(OrderStatus::Cancelled.is_terminal());
        assert!(OrderStatus::TimedOut.is_terminal());
        assert!(!OrderStatus::PartiallyFilled.is_terminal());
        assert!(OrderStatus::PartiallyFilled.is_cancellable());
        assert!(!OrderStatus::Filled.is_cancellable());
    }

    #[test]
    fn reservation_release_never_negative() {
        let mut reservation = Reservation::new(BigDecimal::from(10), 1);
        reservation.consumed_amount = BigDecimal::from(4);
        assert_eq!(reservation.releasable_amount(), BigDecimal::from(6));

        // Defensive: an over-consumed reservation releases zero, not a
        // negative amount.
        reservation.consumed_amount = BigDecimal::from(12);
        assert_eq!(reservation.releasable_amount(), BigDecimal::from(0));
    }

    #[test]
    fn order_status_round_trips_through_string() {
        for status in [
            OrderStatus::Pending,
            OrderStatus::Executing,
            OrderStatus::PartiallyFilled,
            OrderStatus::Filled,
            OrderStatus::Cancelled,
            OrderStatus::Expired,
            OrderStatus::TimedOut,
            OrderStatus::Failed,
        ] {
            assert_eq!(status.as_str().parse::<OrderStatus>().unwrap(), status);
        }
        assert!("nonsense".parse::<OrderStatus>().is_err());
    }

    #[test]
    fn source_kind_round_trips_through_string() {
        for kind in [
            LiquiditySourceKind::StellarDex,
            LiquiditySourceKind::AnchorPlatform,
            LiquiditySourceKind::InternalBook,
        ] {
            assert_eq!(kind.as_str().parse::<LiquiditySourceKind>().unwrap(), kind);
        }
    }

    #[test]
    fn order_leg_remaining_is_clamped() {
        let leg = OrderLeg {
            id: Uuid::nil(),
            quote_leg_id: None,
            source_id: "s".into(),
            source_kind: LiquiditySourceKind::StellarDex,
            priority: 0,
            allocated_sell_amount: BigDecimal::from(5),
            filled_sell_amount: BigDecimal::from(7),
            filled_buy_amount: BigDecimal::from(0),
            average_fill_price: BigDecimal::from(0),
            quoted_price: BigDecimal::from(1),
            fee_bps: 0,
            fill_count: 1,
            status: LegStatus::Filled,
        };
        assert_eq!(leg.remaining_sell_amount(), BigDecimal::from(0));
    }
}
