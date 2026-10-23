//! Swap quote/order service.
//!
//! The service owns the split-allocation and fill-accounting state machine.
//! All mutations happen under one write lock, with no awaits while the lock
//! is held, so a fill cannot race a cancellation and release a reservation
//! twice.
//!
//! Handlers use [`global`], a process-wide instance seeded from the
//! environment. Tests construct their own [`SwapService`] with a deterministic
//! clock value passed to every method.

use bigdecimal::BigDecimal;
use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;
use std::sync::OnceLock;
use uuid::Uuid;

use super::fill_allocation::{allocate_deterministic, from_units, quantize_down, to_units};
use super::types::{
    Fill, LegStatus, OrderLeg, OrderStatus, QuoteLeg, QuoteStatus, ReleaseReason, ReleaseRecord,
    Reservation, SwapOrder, SwapOrderError, SwapOrderResult, SwapQuote, SwapQuoteRequest,
};

const DEFAULT_QUOTE_TTL_SECS: u64 = 30;
const DEFAULT_MAX_QUOTE_TTL_SECS: u64 = 300;
const DEFAULT_EXECUTION_TIMEOUT_SECS: u64 = 120;
const DEFAULT_LIMIT_BUDGET: u32 = 1_000;
const DEFAULT_MAX_SOURCES: usize = 16;
const MAX_ASSET_LEN: usize = 64;
const MAX_CLIENT_ORDER_ID_LEN: usize = 128;

#[derive(Debug, Clone)]
pub struct SwapConfig {
    pub default_quote_ttl_secs: u64,
    pub max_quote_ttl_secs: u64,
    pub execution_timeout_secs: u64,
    /// Maximum number of live quotes/orders one tenant may hold. The budget is
    /// reserved at quote creation and consumed permanently by a successful
    /// order; cancellation, expiry, and timeout release it.
    pub limit_budget: u32,
    pub max_sources_per_quote: usize,
}

impl Default for SwapConfig {
    fn default() -> Self {
        Self {
            default_quote_ttl_secs: DEFAULT_QUOTE_TTL_SECS,
            max_quote_ttl_secs: DEFAULT_MAX_QUOTE_TTL_SECS,
            execution_timeout_secs: DEFAULT_EXECUTION_TIMEOUT_SECS,
            limit_budget: DEFAULT_LIMIT_BUDGET,
            max_sources_per_quote: DEFAULT_MAX_SOURCES,
        }
    }
}

impl SwapConfig {
    pub fn from_env() -> Self {
        let default = Self::default();
        Self {
            default_quote_ttl_secs: env_u64(
                "SWAP_QUOTE_TTL_SECS",
                default.default_quote_ttl_secs,
            )
            .max(1),
            max_quote_ttl_secs: env_u64("SWAP_QUOTE_MAX_TTL_SECS", default.max_quote_ttl_secs)
                .max(1),
            execution_timeout_secs: env_u64(
                "SWAP_EXECUTION_TIMEOUT_SECS",
                default.execution_timeout_secs,
            )
            .max(1),
            limit_budget: env_u64("SWAP_LIMIT_BUDGET", u64::from(default.limit_budget))
                .min(u64::from(u32::MAX)) as u32,
            max_sources_per_quote: env_u64(
                "SWAP_MAX_SOURCES_PER_QUOTE",
                default.max_sources_per_quote as u64,
            )
            .clamp(1, 64) as usize,
        }
    }
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

#[derive(Debug, Default)]
struct SwapState {
    quotes: HashMap<Uuid, SwapQuote>,
    orders: HashMap<Uuid, SwapOrder>,
    /// `(tenant_id, client_order_id)` index for idempotent order creation.
    client_orders: HashMap<(Uuid, String), Uuid>,
    used_limit_budget: u32,
}

pub struct SwapService {
    state: tokio::sync::RwLock<SwapState>,
    config: SwapConfig,
}

impl SwapService {
    pub fn new(config: SwapConfig) -> Self {
        Self {
            state: tokio::sync::RwLock::new(SwapState::default()),
            config,
        }
    }

    pub fn config(&self) -> &SwapConfig {
        &self.config
    }

    pub async fn limit_budget_available(&self) -> u32 {
        let state = self.state.read().await;
        self.config
            .limit_budget
            .saturating_sub(state.used_limit_budget)
    }

    /// Create a quote, deterministically splitting `sell_amount` across the
    /// supplied sources.
    pub async fn create_quote(
        &self,
        tenant_id: Uuid,
        request: SwapQuoteRequest,
        now: DateTime<Utc>,
    ) -> SwapOrderResult<SwapQuote> {
        self.validate_quote_request(&request)?;

        let sell_amount = quantize_down(&request.sell_amount);
        let sell_units = to_units(&sell_amount)?;
        let allocations = allocate_deterministic(sell_units, &request.sources)?;

        let source_by_id: HashMap<&str, &super::types::LiquiditySource> = request
            .sources
            .iter()
            .map(|source| (source.id.as_str(), source))
            .collect();

        let mut legs = Vec::with_capacity(allocations.len());
        let mut total_buy = BigDecimal::from(0);

        for allocation in &allocations {
            let source = source_by_id
                .get(allocation.source_id.as_str())
                .ok_or_else(|| {
                    SwapOrderError::Internal(format!(
                        "allocated source {} not present in quote request",
                        allocation.source_id
                    ))
                })?;

            let leg_sell = from_units(allocation.amount_units);
            let gross_buy = quantize_down(&(&leg_sell * &source.price));
            let net_buy = apply_fee(&gross_buy, source.fee_bps);
            total_buy = total_buy + &net_buy;

            legs.push(QuoteLeg {
                id: Uuid::new_v4(),
                source_id: source.id.clone(),
                source_kind: source.kind,
                priority: allocation.priority,
                sell_amount: leg_sell,
                price: source.price.clone(),
                expected_buy_amount: net_buy,
                fee_bps: source.fee_bps,
            });
        }

        let average_price = if sell_amount > BigDecimal::from(0) {
            quantize_down(&(&total_buy / &sell_amount))
        } else {
            BigDecimal::from(0)
        };

        let ttl_secs = request
            .ttl_seconds
            .unwrap_or(self.config.default_quote_ttl_secs)
            .clamp(1, self.config.max_quote_ttl_secs);

        let quote = SwapQuote {
            id: Uuid::new_v4(),
            tenant_id,
            sell_asset: request.sell_asset.trim().to_ascii_uppercase(),
            buy_asset: request.buy_asset.trim().to_ascii_uppercase(),
            sell_amount: sell_amount.clone(),
            estimated_buy_amount: total_buy,
            average_price,
            limit_price: request.limit_price.clone(),
            legs,
            status: QuoteStatus::Active,
            reservation: Reservation::new(sell_amount, 1),
            created_at: now,
            expires_at: now + Duration::seconds(ttl_secs as i64),
            released_at: None,
        };

        let mut state = self.state.write().await;
        if state.used_limit_budget >= self.config.limit_budget {
            return Err(SwapOrderError::LimitBudgetExhausted);
        }
        state.used_limit_budget += 1;
        state.quotes.insert(quote.id, quote.clone());
        Ok(quote)
    }

    pub async fn get_quote(
        &self,
        tenant_id: Uuid,
        quote_id: Uuid,
    ) -> SwapOrderResult<SwapQuote> {
        let state = self.state.read().await;
        state
            .quotes
            .get(&quote_id)
            .filter(|quote| quote.tenant_id == tenant_id)
            .cloned()
            .ok_or(SwapOrderError::NotFound)
    }

    /// Create an order from an active quote. Repeating the same
    /// `client_order_id` for the same tenant returns the original order, even
    /// if the quote has since been consumed.
    pub async fn create_order(
        &self,
        tenant_id: Uuid,
        quote_id: Uuid,
        client_order_id: Option<String>,
        now: DateTime<Utc>,
    ) -> SwapOrderResult<SwapOrder> {
        let client_order_id = normalize_client_order_id(client_order_id)?;

        let mut state = self.state.write().await;

        if let Some(client_id) = &client_order_id {
            if let Some(existing_id) = state
                .client_orders
                .get(&(tenant_id, client_id.clone()))
                .copied()
            {
                if let Some(existing) = state.orders.get(&existing_id) {
                    return Ok(existing.clone());
                }
            }
        }

        let quote = state
            .quotes
            .get(&quote_id)
            .filter(|quote| quote.tenant_id == tenant_id)
            .cloned()
            .ok_or(SwapOrderError::NotFound)?;

        match quote.status {
            QuoteStatus::Consumed => return Err(SwapOrderError::QuoteConsumed),
            QuoteStatus::Expired => return Err(SwapOrderError::QuoteExpired),
            QuoteStatus::Active => {}
        }

        if quote.is_expired_at(now) {
            expire_quote_locked(&mut state, quote_id, now);
            return Err(SwapOrderError::QuoteExpired);
        }

        let order = SwapOrder {
            id: Uuid::new_v4(),
            tenant_id,
            quote_id,
            client_order_id: client_order_id.clone(),
            sell_asset: quote.sell_asset.clone(),
            buy_asset: quote.buy_asset.clone(),
            sell_amount: quote.sell_amount.clone(),
            estimated_buy_amount: quote.estimated_buy_amount.clone(),
            filled_sell_amount: BigDecimal::from(0),
            filled_buy_amount: BigDecimal::from(0),
            limit_price: quote.limit_price.clone(),
            status: OrderStatus::Pending,
            legs: quote
                .legs
                .iter()
                .map(|leg| OrderLeg {
                    id: Uuid::new_v4(),
                    quote_leg_id: Some(leg.id),
                    source_id: leg.source_id.clone(),
                    source_kind: leg.source_kind,
                    priority: leg.priority,
                    allocated_sell_amount: leg.sell_amount.clone(),
                    filled_sell_amount: BigDecimal::from(0),
                    filled_buy_amount: BigDecimal::from(0),
                    average_fill_price: BigDecimal::from(0),
                    quoted_price: leg.price.clone(),
                    fee_bps: leg.fee_bps,
                    fill_count: 0,
                    status: LegStatus::Pending,
                })
                .collect(),
            fills: Vec::new(),
            reservation: quote.reservation.clone(),
            release: None,
            created_at: now,
            updated_at: now,
            execution_deadline: now + Duration::seconds(self.config.execution_timeout_secs as i64),
        };

        if let Some(client_id) = client_order_id {
            state.client_orders.insert((tenant_id, client_id), order.id);
        }
        if let Some(stored_quote) = state.quotes.get_mut(&quote_id) {
            stored_quote.status = QuoteStatus::Consumed;
        }
        state.orders.insert(order.id, order.clone());
        Ok(order)
    }

    pub async fn get_order(
        &self,
        tenant_id: Uuid,
        order_id: Uuid,
    ) -> SwapOrderResult<SwapOrder> {
        let state = self.state.read().await;
        state
            .orders
            .get(&order_id)
            .filter(|order| order.tenant_id == tenant_id)
            .cloned()
            .ok_or(SwapOrderError::NotFound)
    }

    /// Report one fill for one leg. Fills are additive; the order is
    /// `partially_filled` until every leg reaches its allocation and `filled`
    /// once they all do. A repeated `venue_reference` with identical details
    /// is idempotent.
    pub async fn record_fill(
        &self,
        tenant_id: Uuid,
        order_id: Uuid,
        leg_id: Uuid,
        sell_amount: BigDecimal,
        price: BigDecimal,
        venue_reference: Option<String>,
        now: DateTime<Utc>,
    ) -> SwapOrderResult<(SwapOrder, Fill)> {
        let sell_amount = quantize_down(&sell_amount);
        let amount_units = to_units(&sell_amount)?;
        if amount_units <= 0 {
            return Err(SwapOrderError::Validation(
                "fill amount must be positive at 7dp precision".to_string(),
            ));
        }
        if price <= BigDecimal::from(0) {
            return Err(SwapOrderError::Validation(
                "fill price must be positive".to_string(),
            ));
        }

        let mut state = self.state.write().await;

        let order = state
            .orders
            .get(&order_id)
            .filter(|order| order.tenant_id == tenant_id)
            .ok_or(SwapOrderError::NotFound)?;

        if order.status.is_terminal() {
            return Err(SwapOrderError::InvalidState {
                from: order.status,
                to: OrderStatus::PartiallyFilled,
            });
        }
        if now >= order.execution_deadline {
            release_order_locked(
                &mut state,
                order_id,
                ReleaseReason::ExecutionTimedOut,
                now,
            );
            return Err(SwapOrderError::ExecutionTimedOut);
        }

        if let Some(reference) = &venue_reference {
            if let Some(existing) = order
                .fills
                .iter()
                .find(|fill| fill.venue_reference.as_deref() == Some(reference.as_str()))
            {
                let same = existing.leg_id == leg_id
                    && to_units(&existing.sell_amount).ok() == Some(amount_units)
                    && existing.price == price;
                if same {
                    return Ok((order.clone(), existing.clone()));
                }
                return Err(SwapOrderError::DuplicateFill(reference.clone()));
            }
        }

        let leg_index = order
            .legs
            .iter()
            .position(|leg| leg.id == leg_id)
            .ok_or_else(|| {
                SwapOrderError::Validation(format!("unknown leg {leg_id} on order {order_id}"))
            })?;
        let allocated_units = to_units(&order.legs[leg_index].allocated_sell_amount)?;
        let filled_units = to_units(&order.legs[leg_index].filled_sell_amount)?;
        let remaining_units = allocated_units.saturating_sub(filled_units);
        if amount_units > remaining_units {
            return Err(SwapOrderError::Overfill {
                leg_id,
                requested: sell_amount,
                remaining: from_units(remaining_units),
            });
        }

        let gross_buy = quantize_down(&(&sell_amount * &price));
        let buy_amount = apply_fee(&gross_buy, order.legs[leg_index].fee_bps);
        let fill = Fill {
            id: Uuid::new_v4(),
            order_id,
            leg_id,
            source_id: order.legs[leg_index].source_id.clone(),
            sell_amount: sell_amount.clone(),
            buy_amount: buy_amount.clone(),
            price: price.clone(),
            venue_reference,
            filled_at: now,
        };

        // Re-borrow mutably now that all lookups/validations are done.
        let order = state
            .orders
            .get_mut(&order_id)
            .expect("order existence checked above");
        order.fills.push(fill.clone());

        let leg = &mut order.legs[leg_index];
        leg.filled_sell_amount = &leg.filled_sell_amount + &sell_amount;
        leg.filled_buy_amount = &leg.filled_buy_amount + &buy_amount;
        leg.fill_count += 1;
        leg.average_fill_price = weighted_average_price(&order.fills, leg_id);
        leg.status = if to_units(&leg.filled_sell_amount)? >= allocated_units {
            LegStatus::Filled
        } else {
            LegStatus::PartiallyFilled
        };

        order.filled_sell_amount = &order.filled_sell_amount + &sell_amount;
        order.filled_buy_amount = &order.filled_buy_amount + &buy_amount;
        order.reservation.consumed_amount =
            &order.reservation.consumed_amount + &sell_amount;
        order.updated_at = now;

        let total_units = to_units(&order.sell_amount)?;
        order.status = if to_units(&order.filled_sell_amount)? >= total_units {
            order.reservation.consumed_limit_budget = order.reservation.reserved_limit_budget;
            OrderStatus::Filled
        } else {
            OrderStatus::PartiallyFilled
        };

        let updated = order.clone();
        Ok((updated, fill))
    }

    /// Snapshot of every known order, oldest first. Reconciliation consumes
    /// this; deterministic ordering keeps reports reproducible.
    pub async fn all_orders(&self) -> Vec<SwapOrder> {
        let state = self.state.read().await;
        let mut orders: Vec<SwapOrder> = state.orders.values().cloned().collect();
        orders.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
        orders
    }

    pub async fn orders_between(
        &self,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Vec<SwapOrder> {
        self.all_orders()
            .await
            .into_iter()
            .filter(|order| order.created_at >= start && order.created_at < end)
            .collect()
    }

    pub async fn all_quotes(&self) -> Vec<SwapQuote> {
        let state = self.state.read().await;
        let mut quotes: Vec<SwapQuote> = state.quotes.values().cloned().collect();
        quotes.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
        quotes
    }

    fn validate_quote_request(&self, request: &SwapQuoteRequest) -> SwapOrderResult<()> {
        let sell_asset = request.sell_asset.trim();
        let buy_asset = request.buy_asset.trim();
        if sell_asset.is_empty() || buy_asset.is_empty() {
            return Err(SwapOrderError::Validation(
                "sell_asset and buy_asset are required".to_string(),
            ));
        }
        if sell_asset.len() > MAX_ASSET_LEN || buy_asset.len() > MAX_ASSET_LEN {
            return Err(SwapOrderError::Validation(format!(
                "asset codes must be at most {MAX_ASSET_LEN} characters"
            )));
        }
        if sell_asset.eq_ignore_ascii_case(buy_asset) {
            return Err(SwapOrderError::Validation(
                "sell_asset and buy_asset must differ".to_string(),
            ));
        }
        if request.sell_amount <= BigDecimal::from(0) {
            return Err(SwapOrderError::Validation(
                "sell_amount must be positive".to_string(),
            ));
        }
        if let Some(limit_price) = &request.limit_price {
            if limit_price <= &BigDecimal::from(0) {
                return Err(SwapOrderError::Validation(
                    "limit_price must be positive".to_string(),
                ));
            }
        }
        if request.sources.is_empty() {
            return Err(SwapOrderError::Validation(
                "at least one liquidity source is required".to_string(),
            ));
        }
        if request.sources.len() > self.config.max_sources_per_quote {
            return Err(SwapOrderError::Validation(format!(
                "at most {} liquidity sources may be supplied per quote",
                self.config.max_sources_per_quote
            )));
        }
        Ok(())
    }
}

fn normalize_client_order_id(
    client_order_id: Option<String>,
) -> SwapOrderResult<Option<String>> {
    match client_order_id {
        Some(value) => {
            let trimmed = value.trim().to_string();
            if trimmed.is_empty() {
                return Err(SwapOrderError::Validation(
                    "client_order_id must not be empty".to_string(),
                ));
            }
            if trimmed.len() > MAX_CLIENT_ORDER_ID_LEN {
                return Err(SwapOrderError::Validation(format!(
                    "client_order_id must be at most {MAX_CLIENT_ORDER_ID_LEN} characters"
                )));
            }
            Ok(Some(trimmed))
        }
        None => Ok(None),
    }
}

fn apply_fee(gross_buy: &BigDecimal, fee_bps: u32) -> BigDecimal {
    if fee_bps == 0 {
        return gross_buy.clone();
    }
    let net = (gross_buy * BigDecimal::from(10_000 - fee_bps.min(10_000)))
        / BigDecimal::from(10_000);
    quantize_down(&net)
}

fn weighted_average_price(fills: &[Fill], leg_id: Uuid) -> BigDecimal {
    let mut total_notional = BigDecimal::from(0);
    let mut total_amount = BigDecimal::from(0);
    for fill in fills.iter().filter(|fill| fill.leg_id == leg_id) {
        total_notional = total_notional + &fill.sell_amount * &fill.price;
        total_amount = total_amount + &fill.sell_amount;
    }
    if total_amount > BigDecimal::from(0) {
        quantize_down(&(total_notional / total_amount))
    } else {
        BigDecimal::from(0)
    }
}

/// Expire a quote and release the budget it reserved. Idempotent: a quote that
/// is already consumed/expired is left untouched and releases nothing.
fn expire_quote_locked(state: &mut SwapState, quote_id: Uuid, now: DateTime<Utc>) {
    let was_active = {
        let quote = match state.quotes.get_mut(&quote_id) {
            Some(quote) => quote,
            None => return,
        };
        if quote.status != QuoteStatus::Active {
            false
        } else {
            quote.status = QuoteStatus::Expired;
            quote.released_at = Some(now);
            true
        }
    };
    if was_active {
        state.used_limit_budget = state.used_limit_budget.saturating_sub(1);
    }
}

/// Transition an order to a terminal release state and return exactly the
/// unused reservation. Safe to call more than once: an existing release is
/// never replaced.
fn release_order_locked(
    state: &mut SwapState,
    order_id: Uuid,
    reason: ReleaseReason,
    now: DateTime<Utc>,
) -> Option<ReleaseRecord> {
    let order = state.orders.get_mut(&order_id)?;
    if order.status.is_terminal() {
        return order.release.clone();
    }

    let release = ReleaseRecord {
        reason,
        amount: order.reservation.releasable_amount(),
        limit_budget: order.reservation.releasable_limit_budget(),
        released_at: now,
    };

    order.status = match reason {
        ReleaseReason::ClientCancelled => OrderStatus::Cancelled,
        ReleaseReason::QuoteExpired => OrderStatus::Expired,
        ReleaseReason::ExecutionTimedOut => OrderStatus::TimedOut,
        ReleaseReason::OrderFailed => OrderStatus::Failed,
    };
    order.release = Some(release.clone());
    order.updated_at = now;
    for leg in &mut order.legs {
        if leg.status != LegStatus::Filled {
            leg.status = LegStatus::Released;
        }
    }

    let budget = release.limit_budget;
    state.used_limit_budget = state.used_limit_budget.saturating_sub(budget);
    Some(release)
}

/// Process-wide swap service used by HTTP handlers.
pub fn global() -> &'static SwapService {
    static SERVICE: OnceLock<SwapService> = OnceLock::new();
    SERVICE.get_or_init(|| SwapService::new(SwapConfig::from_env()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swap::types::LiquiditySourceKind;

    fn source(id: &str, price: i64, available_units: i64) -> super::super::types::LiquiditySource {
        super::super::types::LiquiditySource {
            id: id.to_string(),
            kind: LiquiditySourceKind::StellarDex,
            price: BigDecimal::from(price),
            available: from_units(available_units),
            weight: 1,
            priority: 0,
            min_leg_amount: None,
            max_leg_amount: None,
            fee_bps: 0,
        }
    }

    fn request(sources: Vec<super::super::types::LiquiditySource>) -> SwapQuoteRequest {
        SwapQuoteRequest {
            sell_asset: "USDC".to_string(),
            buy_asset: "XLM".to_string(),
            sell_amount: from_units(100_000_000),
            limit_price: None,
            ttl_seconds: Some(30),
            sources,
        }
    }

    fn service() -> SwapService {
        SwapService::new(SwapConfig {
            limit_budget: 10,
            execution_timeout_secs: 60,
            ..SwapConfig::default()
        })
    }

    #[tokio::test]
    async fn quote_splits_sell_amount_exactly() {
        let service = service();
        let tenant = Uuid::new_v4();
        let quote = service
            .create_quote(
                tenant,
                request(vec![
                    source("a", 2, 1_000_000_000),
                    source("b", 3, 1_000_000_000),
                ]),
                Utc::now(),
            )
            .await
            .unwrap();

        assert_eq!(quote.legs.len(), 2);
        let total: BigDecimal = quote
            .legs
            .iter()
            .fold(BigDecimal::from(0), |sum, leg| sum + &leg.sell_amount);
        assert_eq!(total, quote.sell_amount);
        // 10 sell units split 50/50 by weight: 5 + 5.
        assert_eq!(quote.legs[0].sell_amount, from_units(50_000_000));
        assert_eq!(quote.legs[1].sell_amount, from_units(50_000_000));
        assert_eq!(quote.estimated_buy_amount, BigDecimal::from(25));
    }

    #[tokio::test]
    async fn order_creation_consumes_quote_and_reserves_funds() {
        let service = service();
        let tenant = Uuid::new_v4();
        let quote = service
            .create_quote(
                tenant,
                request(vec![source("a", 1, 1_000_000_000)]),
                Utc::now(),
            )
            .await
            .unwrap();

        let order = service
            .create_order(tenant, quote.id, None, Utc::now())
            .await
            .unwrap();
        assert_eq!(order.status, OrderStatus::Pending);
        assert_eq!(order.sell_amount, quote.sell_amount);
        assert_eq!(order.reservation.reserved_amount, quote.sell_amount);

        assert!(matches!(
            service
                .create_order(tenant, quote.id, None, Utc::now())
                .await,
            Err(SwapOrderError::QuoteConsumed)
        ));

        // A different tenant cannot see the order.
        let other = Uuid::new_v4();
        assert!(matches!(
            service.get_order(other, order.id).await,
            Err(SwapOrderError::NotFound)
        ));
    }

    #[tokio::test]
    async fn client_order_id_is_idempotent() {
        let service = service();
        let tenant = Uuid::new_v4();
        let quote = service
            .create_quote(
                tenant,
                request(vec![source("a", 1, 1_000_000_000)]),
                Utc::now(),
            )
            .await
            .unwrap();

        let first = service
            .create_order(tenant, quote.id, Some("client-1".into()), Utc::now())
            .await
            .unwrap();
        let second = service
            .create_order(tenant, quote.id, Some("client-1".into()), Utc::now())
            .await
            .unwrap();
        assert_eq!(first.id, second.id);
    }

    #[tokio::test]
    async fn partial_fill_is_reported_per_leg() {
        let service = service();
        let tenant = Uuid::new_v4();
        let quote = service
            .create_quote(
                tenant,
                request(vec![
                    source("a", 2, 1_000_000_000),
                    source("b", 2, 1_000_000_000),
                ]),
                Utc::now(),
            )
            .await
            .unwrap();
        let order = service
            .create_order(tenant, quote.id, None, Utc::now())
            .await
            .unwrap();

        let leg_id = order.legs[0].id;
        let (order, fill) = service
            .record_fill(
                tenant,
                order.id,
                leg_id,
                from_units(20_000_000),
                BigDecimal::from(2),
                Some("venue-1".into()),
                Utc::now(),
            )
            .await
            .unwrap();

        assert_eq!(fill.sell_amount, from_units(20_000_000));
        assert_eq!(order.status, OrderStatus::PartiallyFilled);
        assert_eq!(order.filled_sell_amount, from_units(20_000_000));
        assert_eq!(order.legs[0].status, LegStatus::PartiallyFilled);
        assert_eq!(order.legs[1].status, LegStatus::Pending);
        assert_eq!(
            order.legs[0].remaining_sell_amount(),
            from_units(30_000_000)
        );
        assert_eq!(order.legs[0].average_fill_price, BigDecimal::from(2));
        assert_eq!(order.legs[0].fill_count, 1);
    }

    #[tokio::test]
    async fn overfill_is_rejected() {
        let service = service();
        let tenant = Uuid::new_v4();
        let quote = service
            .create_quote(
                tenant,
                request(vec![source("a", 1, 1_000_000_000)]),
                Utc::now(),
            )
            .await
            .unwrap();
        let order = service
            .create_order(tenant, quote.id, None, Utc::now())
            .await
            .unwrap();

        let error = service
            .record_fill(
                tenant,
                order.id,
                order.legs[0].id,
                from_units(110_000_000),
                BigDecimal::from(1),
                None,
                Utc::now(),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, SwapOrderError::Overfill { .. }));
    }

    #[tokio::test]
    async fn duplicate_venue_reference_is_idempotent_only_for_identical_fill() {
        let service = service();
        let tenant = Uuid::new_v4();
        let quote = service
            .create_quote(
                tenant,
                request(vec![source("a", 1, 1_000_000_000)]),
                Utc::now(),
            )
            .await
            .unwrap();
        let order = service
            .create_order(tenant, quote.id, None, Utc::now())
            .await
            .unwrap();

        let (_, first) = service
            .record_fill(
                tenant,
                order.id,
                order.legs[0].id,
                from_units(10_000_000),
                BigDecimal::from(1),
                Some("ref-1".into()),
                Utc::now(),
            )
            .await
            .unwrap();
        let (_, second) = service
            .record_fill(
                tenant,
                order.id,
                order.legs[0].id,
                from_units(10_000_000),
                BigDecimal::from(1),
                Some("ref-1".into()),
                Utc::now(),
            )
            .await
            .unwrap();
        assert_eq!(first.id, second.id);

        let error = service
            .record_fill(
                tenant,
                order.id,
                order.legs[0].id,
                from_units(20_000_000),
                BigDecimal::from(1),
                Some("ref-1".into()),
                Utc::now(),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, SwapOrderError::DuplicateFill(_)));
    }

    #[tokio::test]
    async fn filling_every_leg_completes_the_order() {
        let service = service();
        let tenant = Uuid::new_v4();
        let quote = service
            .create_quote(
                tenant,
                request(vec![
                    source("a", 1, 1_000_000_000),
                    source("b", 1, 1_000_000_000),
                ]),
                Utc::now(),
            )
            .await
            .unwrap();
        let order = service
            .create_order(tenant, quote.id, None, Utc::now())
            .await
            .unwrap();

        for leg in &order.legs {
            service
                .record_fill(
                    tenant,
                    order.id,
                    leg.id,
                    leg.remaining_sell_amount(),
                    BigDecimal::from(1),
                    None,
                    Utc::now(),
                )
                .await
                .unwrap();
        }

        let order = service.get_order(tenant, order.id).await.unwrap();
        assert_eq!(order.status, OrderStatus::Filled);
        assert!(order.legs.iter().all(|leg| leg.status == LegStatus::Filled));
        assert_eq!(order.filled_sell_amount, order.sell_amount);
        assert_eq!(order.reservation.releasable_amount(), BigDecimal::from(0));
        assert_eq!(order.reservation.releasable_limit_budget(), 0);
    }

    #[tokio::test]
    async fn expired_quote_is_rejected_and_budget_released() {
        let service = service();
        let tenant = Uuid::new_v4();
        let start = Utc::now();
        let quote = service
            .create_quote(
                tenant,
                request(vec![source("a", 1, 1_000_000_000)]),
                start,
            )
            .await
            .unwrap();
        assert_eq!(service.limit_budget_available().await, 9);

        let error = service
            .create_order(
                tenant,
                quote.id,
                None,
                start + Duration::seconds(31),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, SwapOrderError::QuoteExpired));
        assert_eq!(service.limit_budget_available().await, 10);
        assert_eq!(
            service.get_quote(tenant, quote.id).await.unwrap().status,
            QuoteStatus::Expired
        );
    }

    #[tokio::test]
    async fn tenant_limit_budget_is_enforced() {
        let service = SwapService::new(SwapConfig {
            limit_budget: 1,
            ..SwapConfig::default()
        });
        let tenant = Uuid::new_v4();
        service
            .create_quote(
                tenant,
                request(vec![source("a", 1, 1_000_000_000)]),
                Utc::now(),
            )
            .await
            .unwrap();
        let error = service
            .create_quote(
                tenant,
                request(vec![source("a", 1, 1_000_000_000)]),
                Utc::now(),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, SwapOrderError::LimitBudgetExhausted));
    }
}
