//! Internal order crossing and netting between tenant flows.
//!
//! Detects offsetting buy/sell interest for the same asset pair within a short
//! window and crosses them internally after risk checks, so both orders settle
//! at an improved price without paying venue spread and network fees.
//!
//! Design notes:
//! - The index is a short-lived in-memory view of pending opposing interest,
//!   backed by persisted order state so a restart does not silently drop
//!   eligible orders.
//! - Matching runs in the same worker context as execution to avoid a second
//!   source of truth.
//! - Crossed fills emit the same domain events and persistence records as
//!   external fills, with the counterparty venue attributed as internal.
//! - Confirmation is two-phase: neither side is committed until both accept.
//!   Any failure or cancellation falls back to normal external execution.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::swap::execution::{ExecutionError, ExecutionResult, Fill, FillVenue, OrderSide};

/// Venue attribution used for internally crossed fills.
pub const INTERNAL_VENUE: &str = "internal";

/// Configuration for the internal crossing engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossingConfig {
    /// Feature flag for staged enablement. When `false` no crossing is attempted.
    pub enabled: bool,
    /// Window during which eligible opposing orders may be matched.
    pub window: Duration,
    /// Tenants that have opted out of internal crossing entirely.
    pub opted_out_tenants: Vec<String>,
}

impl Default for CrossingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            window: Duration::from_millis(500),
            opted_out_tenants: Vec::new(),
        }
    }
}

impl CrossingConfig {
    /// Returns `true` when the given tenant is eligible to participate in crossing.
    pub fn tenant_eligible(&self, tenant_id: &str) -> bool {
        self.enabled && !self.opted_out_tenants.iter().any(|t| t == tenant_id)
    }
}

/// A pending order that may participate in an internal cross.
#[derive(Debug, Clone)]
pub struct PendingInterest {
    pub order_id: String,
    pub tenant_id: String,
    /// Linked account identifier used for self-trade prevention.
    pub linked_account: Option<String>,
    pub asset_pair: String,
    pub side: OrderSide,
    pub quantity: Decimal,
    /// Limit price the tenant is willing to accept.
    pub limit_price: Decimal,
    pub submitted_at: Instant,
}

/// Quotes used to bound the matching price and enforce best execution.
#[derive(Debug, Clone)]
pub struct CrossQuotes {
    /// Best external bid (price at which we could sell externally).
    pub external_bid: Decimal,
    /// Best external ask (price at which we could buy externally).
    pub external_ask: Decimal,
    /// Aggregated/oracle mid price used to bound the cross.
    pub oracle_mid: Decimal,
    /// Maximum allowed deviation from the oracle mid, as a fraction (e.g. 0.01 = 1%).
    pub max_oracle_deviation: Decimal,
}

/// Outcome of a crossing attempt, recorded for audit purposes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossAudit {
    pub asset_pair: String,
    pub buy_order_id: String,
    pub sell_order_id: String,
    pub buy_tenant_id: String,
    pub sell_tenant_id: String,
    pub quantity: Decimal,
    pub price: Decimal,
    pub external_bid: Decimal,
    pub external_ask: Decimal,
    pub oracle_mid: Decimal,
    pub completed: bool,
    pub reason: Option<String>,
}

/// A proposed cross awaiting two-phase confirmation.
#[derive(Debug, Clone)]
pub struct ProposedCross {
    pub buy: PendingInterest,
    pub sell: PendingInterest,
    pub quantity: Decimal,
    pub price: Decimal,
    pub audit: CrossAudit,
}

/// In-memory index of pending opposing interest per asset pair.
#[derive(Default)]
pub struct CrossingEngine {
    config: CrossingConfig,
    pending: HashMap<String, Vec<PendingInterest>>,
}

impl CrossingEngine {
    pub fn new(config: CrossingConfig) -> Self {
        Self {
            config,
            pending: HashMap::new(),
        }
    }

    pub fn config(&self) -> &CrossingConfig {
        &self.config
    }

    /// Register pending interest. Orders from opted-out tenants are ignored.
    pub fn register(&mut self, interest: PendingInterest) {
        if !self.config.tenant_eligible(&interest.tenant_id) {
            return;
        }
        self.pending
            .entry(interest.asset_pair.clone())
            .or_default()
            .push(interest);
    }

    /// Remove an order from the index (e.g. on cancellation or fill).
    pub fn remove(&mut self, order_id: &str) {
        for orders in self.pending.values_mut() {
            orders.retain(|o| o.order_id != order_id);
        }
    }

    /// Drop interest that has fallen outside the crossing window.
    pub fn prune(&mut self, now: Instant) {
        let window = self.config.window;
        for orders in self.pending.values_mut() {
            orders.retain(|o| now.duration_since(o.submitted_at) <= window);
        }
    }

    /// Attempt to find and price a cross for the given incoming order.
    ///
    /// Returns `None` when no eligible counterparty exists or the best-execution
    /// guard rejects the cross. The returned `ProposedCross` is not yet committed.
    pub fn propose(
        &self,
        incoming: &PendingInterest,
        quotes: &CrossQuotes,
        now: Instant,
    ) -> Option<ProposedCross> {
        if !self.config.tenant_eligible(&incoming.tenant_id) {
            return None;
        }

        let candidates = self.pending.get(&incoming.asset_pair)?;

        // Deterministic price-time priority: best price first, then earliest submission.
        let mut eligible: Vec<&PendingInterest> = candidates
            .iter()
            .filter(|o| o.side != incoming.side)
            .filter(|o| now.duration_since(o.submitted_at) <= self.config.window)
            .filter(|o| !is_linked(incoming, o))
            .collect();

        eligible.sort_by(|a, b| {
            let price_cmp = match incoming.side {
                // Incoming buy wants the lowest sell price.
                OrderSide::Buy => a.limit_price.cmp(&b.limit_price),
                // Incoming sell wants the highest buy price.
                OrderSide::Sell => b.limit_price.cmp(&a.limit_price),
            };
            price_cmp.then_with(|| a.submitted_at.cmp(&b.submitted_at))
        });

        for counterparty in eligible {
            if let Some(cross) = self.try_price(incoming, counterparty, quotes) {
                return Some(cross);
            }
        }
        None
    }

    fn try_price(
        &self,
        incoming: &PendingInterest,
        counterparty: &PendingInterest,
        quotes: &CrossQuotes,
    ) -> Option<ProposedCross> {
        let (buy, sell) = match incoming.side {
            OrderSide::Buy => (incoming, counterparty),
            OrderSide::Sell => (counterparty, incoming),
        };

        // Price must be bounded by the oracle mid band.
        let band = quotes.oracle_mid * quotes.max_oracle_deviation;
        let lower = quotes.oracle_mid - band;
        let upper = quotes.oracle_mid + band;

        // Midpoint of the two limits, clamped into the oracle band.
        let raw = (buy.limit_price + sell.limit_price) / Decimal::from(2);
        let price = raw.max(lower).min(upper);

        // Best-execution guard: the cross must beat the external quote for both sides.
        // Buyer pays less than the external ask; seller receives more than the external bid.
        if price >= quotes.external_ask || price <= quotes.external_bid {
            return None;
        }

        // Both limits must still be satisfied at the crossed price.
        if price > buy.limit_price || price < sell.limit_price {
            return None;
        }

        let quantity = buy.quantity.min(sell.quantity);
        if quantity <= Decimal::ZERO {
            return None;
        }

        let audit = CrossAudit {
            asset_pair: incoming.asset_pair.clone(),
            buy_order_id: buy.order_id.clone(),
            sell_order_id: sell.order_id.clone(),
            buy_tenant_id: buy.tenant_id.clone(),
            sell_tenant_id: sell.tenant_id.clone(),
            quantity,
            price,
            external_bid: quotes.external_bid,
            external_ask: quotes.external_ask,
            oracle_mid: quotes.oracle_mid,
            completed: false,
            reason: None,
        };

        Some(ProposedCross {
            buy: buy.clone(),
            sell: sell.clone(),
            quantity,
            price,
            audit,
        })
    }
}

/// Self-trade prevention: reject when both sides belong to the same tenant or a
/// clearly linked account.
fn is_linked(a: &PendingInterest, b: &PendingInterest) -> bool {
    if a.tenant_id == b.tenant_id {
        return true;
    }
    match (&a.linked_account, &b.linked_account) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// Two-phase confirmation. Neither side is committed until both accept.
///
/// `accept_buy` and `accept_sell` are invoked to obtain each side's acceptance;
/// if either declines or errors, the cross is abandoned and the caller falls
/// back to normal external execution.
pub fn confirm<FB, FS>(
    cross: &ProposedCross,
    accept_buy: FB,
    accept_sell: FS,
) -> Result<CrossAudit, ExecutionError>
where
    FB: FnOnce(&ProposedCross) -> bool,
    FS: FnOnce(&ProposedCross) -> bool,
{
    let mut audit = cross.audit.clone();

    if !accept_buy(cross) {
        audit.reason = Some("buy side declined".to_string());
        return Err(ExecutionError::CrossAbandoned(audit));
    }
    if !accept_sell(cross) {
        audit.reason = Some("sell side declined".to_string());
        return Err(ExecutionError::CrossAbandoned(audit));
    }

    audit.completed = true;
    Ok(audit)
}

/// Build the fill records for a completed cross. These are identical in shape to
/// external fills, with the counterparty venue attributed as internal.
pub fn cross_fills(cross: &ProposedCross) -> (Fill, Fill) {
    let buy_fill = Fill {
        order_id: cross.buy.order_id.clone(),
        asset_pair: cross.buy.asset_pair.clone(),
        side: OrderSide::Buy,
        quantity: cross.quantity,
        price: cross.price,
        venue: FillVenue::Internal(INTERNAL_VENUE.to_string()),
    };
    let sell_fill = Fill {
        order_id: cross.sell.order_id.clone(),
        asset_pair: cross.sell.asset_pair.clone(),
        side: OrderSide::Sell,
        quantity: cross.quantity,
        price: cross.price,
        venue: FillVenue::Internal(INTERNAL_VENUE.to_string()),
    };
    (buy_fill, sell_fill)
}

/// Execute a proposed cross end-to-end: two-phase confirm, then emit fills.
/// On any failure the caller must fall back to external execution.
pub fn execute<FB, FS>(
    cross: &ProposedCross,
    accept_buy: FB,
    accept_sell: FS,
) -> ExecutionResult<(CrossAudit, Fill, Fill)>
where
    FB: FnOnce(&ProposedCross) -> bool,
    FS: FnOnce(&ProposedCross) -> bool,
{
    let audit = confirm(cross, accept_buy, accept_sell)?;
    let (buy_fill, sell_fill) = cross_fills(cross);
    Ok((audit, buy_fill, sell_fill))
}
