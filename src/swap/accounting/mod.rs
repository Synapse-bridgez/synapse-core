//! Cost Basis and Tax Lot Tracking — Issue #1489
//!
//! Tracks acquisition cost basis per tenant and asset at the lot level and
//! records realized gains/losses when lots are consumed.
//!
//! # Model
//!
//! ```text
//! Fill event (buy)  ──►  TaxLot (acquisition record)
//! Fill event (sell) ──►  LotConsumption(s) + RealizedPnl entries
//! Compensation/refund ─►  Reversal of existing entries
//! ```
//!
//! # Lot-selection strategies
//!
//! The per-tenant [`LotMethod`] is a pluggable trait so methods are
//! individually unit-testable and new methods can be added without
//! touching the core accounting logic.
//!
//! Supported methods: FIFO, LIFO, HIFO, SpecificLot.
//!
//! # Precision policy
//!
//! All quantities use [`BigDecimal`] with 7 decimal places (Stellar stroop
//! precision). Rounding uses `ROUND_HALF_UP` at every intermediate step to
//! prevent cumulative drift.

use bigdecimal::{BigDecimal, RoundingMode};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use thiserror::Error;
use uuid::Uuid;

// ── Constants ────────────────────────────────────────────────────────────────

/// Decimal scale used throughout: 7 places (Stellar stroop precision).
const SCALE: u64 = 7;

// ── Lot-selection method ─────────────────────────────────────────────────────

/// Lot-selection method for a tenant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LotMethod {
    /// First-In First-Out: oldest lots consumed first.
    Fifo,
    /// Last-In First-Out: newest lots consumed first.
    Lifo,
    /// Highest-In First-Out: highest-cost lots consumed first.
    Hifo,
    /// Specific-lot identification: caller supplies explicit lot IDs.
    SpecificLot,
}

impl Default for LotMethod {
    fn default() -> Self {
        LotMethod::Fifo
    }
}

impl std::fmt::Display for LotMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LotMethod::Fifo => write!(f, "FIFO"),
            LotMethod::Lifo => write!(f, "LIFO"),
            LotMethod::Hifo => write!(f, "HIFO"),
            LotMethod::SpecificLot => write!(f, "SPECIFIC_LOT"),
        }
    }
}

// ── Domain types ─────────────────────────────────────────────────────────────

/// An acquisition lot: one unit of tax-lot accounting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaxLot {
    /// Stable identifier (UUID v4).
    pub id: Uuid,
    /// Owning tenant.
    pub tenant_id: Uuid,
    /// Asset code (e.g. `"USDC"`).
    pub asset_code: String,
    /// Asset issuer G-address. `None` for native XLM.
    pub asset_issuer: Option<String>,
    /// Quantity acquired, in asset decimal units (7 dp).
    pub quantity: BigDecimal,
    /// Remaining quantity not yet consumed.
    pub remaining: BigDecimal,
    /// Cost basis per unit in the reporting currency.
    pub cost_per_unit: BigDecimal,
    /// Total acquisition cost (`quantity * cost_per_unit`).
    pub total_cost: BigDecimal,
    /// Reporting currency code (e.g. `"USD"`).
    pub reporting_currency: String,
    /// Fees paid on acquisition, in the reporting currency.
    pub acquisition_fee: BigDecimal,
    /// Source fill/order event that created this lot.
    pub source_order_id: String,
    /// When the lot was acquired.
    pub acquired_at: DateTime<Utc>,
    /// Whether this lot has been reversed (refund/compensation).
    pub reversed: bool,
}

impl TaxLot {
    /// `true` if the lot has been fully consumed.
    pub fn is_fully_consumed(&self) -> bool {
        self.remaining <= BigDecimal::from(0)
    }
}

/// A single consumption event: some or all of a lot was sold.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LotConsumption {
    pub id: Uuid,
    /// The lot that was consumed.
    pub lot_id: Uuid,
    pub tenant_id: Uuid,
    /// Quantity consumed in this event.
    pub quantity_consumed: BigDecimal,
    /// Cost basis of the consumed quantity (cost_per_unit * quantity_consumed).
    pub cost_basis: BigDecimal,
    /// The fill/order that triggered this consumption.
    pub source_order_id: String,
    /// When the consumption was recorded.
    pub consumed_at: DateTime<Utc>,
    /// Whether this consumption has been reversed.
    pub reversed: bool,
}

/// Realized gain or loss entry derived from lot consumptions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealizedPnl {
    pub id: Uuid,
    pub tenant_id: Uuid,
    /// Asset sold.
    pub asset_code: String,
    pub asset_issuer: Option<String>,
    /// Total proceeds from the sale (in reporting currency).
    pub proceeds: BigDecimal,
    /// Total cost basis of lots consumed.
    pub cost_basis: BigDecimal,
    /// `proceeds - cost_basis`. Positive = gain; negative = loss.
    pub gain_loss: BigDecimal,
    /// Fees paid on the sale (in reporting currency).
    pub sale_fee: BigDecimal,
    /// Lot-selection method used.
    pub lot_method: LotMethod,
    /// IDs of the [`LotConsumption`] entries that make up this PnL record.
    pub consumption_ids: Vec<Uuid>,
    pub reporting_currency: String,
    pub source_order_id: String,
    pub realized_at: DateTime<Utc>,
    /// Whether this entry has been reversed.
    pub reversed: bool,
}

// ── Errors ───────────────────────────────────────────────────────────────────

#[derive(Debug, Error, Clone)]
pub enum AccountingError {
    #[error("insufficient lot quantity: needed {needed}, available {available}")]
    InsufficientLots {
        needed: BigDecimal,
        available: BigDecimal,
    },

    #[error("lot not found: {lot_id}")]
    LotNotFound { lot_id: Uuid },

    #[error("lot already reversed: {lot_id}")]
    AlreadyReversed { lot_id: Uuid },

    #[error("specific-lot IDs required but not provided")]
    SpecificLotIdsRequired,

    #[error("rounding error: {0}")]
    RoundingError(String),

    #[error("cost basis missing for lot {lot_id}: use explicit unknown-lot handling")]
    UnknownCostBasis { lot_id: Uuid },
}

// ── Lot-selection trait ───────────────────────────────────────────────────────

/// Pluggable lot-selection strategy.
///
/// Implementations take a mutable slice of available lots and the quantity to
/// consume, and return a list of `(lot_index, quantity_to_take)` pairs ordered
/// by consumption priority.
pub trait LotSelector: Send + Sync {
    /// Return consumption plan: `(lot_index, quantity_to_take)` for each lot.
    /// `lots` is a slice of available (non-reversed, non-fully-consumed) lots.
    fn select(
        &self,
        lots: &[TaxLot],
        quantity_needed: &BigDecimal,
        specific_ids: Option<&[Uuid]>,
    ) -> Result<Vec<(usize, BigDecimal)>, AccountingError>;
}

// ── FIFO selector ─────────────────────────────────────────────────────────────

/// First-In First-Out lot selector.
pub struct FifoSelector;

impl LotSelector for FifoSelector {
    fn select(
        &self,
        lots: &[TaxLot],
        quantity_needed: &BigDecimal,
        _specific_ids: Option<&[Uuid]>,
    ) -> Result<Vec<(usize, BigDecimal)>, AccountingError> {
        let mut sorted: Vec<usize> = (0..lots.len()).collect();
        sorted.sort_by(|&a, &b| lots[a].acquired_at.cmp(&lots[b].acquired_at));
        greedy_consume(lots, &sorted, quantity_needed)
    }
}

// ── LIFO selector ─────────────────────────────────────────────────────────────

/// Last-In First-Out lot selector.
pub struct LifoSelector;

impl LotSelector for LifoSelector {
    fn select(
        &self,
        lots: &[TaxLot],
        quantity_needed: &BigDecimal,
        _specific_ids: Option<&[Uuid]>,
    ) -> Result<Vec<(usize, BigDecimal)>, AccountingError> {
        let mut sorted: Vec<usize> = (0..lots.len()).collect();
        // newest first
        sorted.sort_by(|&a, &b| lots[b].acquired_at.cmp(&lots[a].acquired_at));
        greedy_consume(lots, &sorted, quantity_needed)
    }
}

// ── HIFO selector ─────────────────────────────────────────────────────────────

/// Highest-In First-Out lot selector.
pub struct HifoSelector;

impl LotSelector for HifoSelector {
    fn select(
        &self,
        lots: &[TaxLot],
        quantity_needed: &BigDecimal,
        _specific_ids: Option<&[Uuid]>,
    ) -> Result<Vec<(usize, BigDecimal)>, AccountingError> {
        let mut sorted: Vec<usize> = (0..lots.len()).collect();
        // highest cost first
        sorted.sort_by(|&a, &b| {
            lots[b]
                .cost_per_unit
                .partial_cmp(&lots[a].cost_per_unit)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        greedy_consume(lots, &sorted, quantity_needed)
    }
}

// ── SpecificLot selector ──────────────────────────────────────────────────────

/// Specific-lot selector: caller supplies the exact lot IDs to consume.
pub struct SpecificLotSelector;

impl LotSelector for SpecificLotSelector {
    fn select(
        &self,
        lots: &[TaxLot],
        quantity_needed: &BigDecimal,
        specific_ids: Option<&[Uuid]>,
    ) -> Result<Vec<(usize, BigDecimal)>, AccountingError> {
        let ids = specific_ids.ok_or(AccountingError::SpecificLotIdsRequired)?;
        // Maintain caller-specified order
        let mut ordered: Vec<usize> = Vec::new();
        for id in ids {
            if let Some(idx) = lots.iter().position(|l| &l.id == id) {
                ordered.push(idx);
            } else {
                return Err(AccountingError::LotNotFound { lot_id: *id });
            }
        }
        greedy_consume(lots, &ordered, quantity_needed)
    }
}

// ── Greedy consumption helper ────────────────────────────────────────────────

/// Walk `order` (indices into `lots`) and take as much as needed from each lot.
fn greedy_consume(
    lots: &[TaxLot],
    order: &[usize],
    quantity_needed: &BigDecimal,
) -> Result<Vec<(usize, BigDecimal)>, AccountingError> {
    let mut remaining_need = quantity_needed.clone();
    let mut plan: Vec<(usize, BigDecimal)> = Vec::new();
    let zero = BigDecimal::from(0);

    for &idx in order {
        if remaining_need <= zero {
            break;
        }
        let lot = &lots[idx];
        if lot.reversed || lot.remaining <= zero {
            continue;
        }
        let take = remaining_need.clone().min(lot.remaining.clone());
        let take = round(take);
        plan.push((idx, take.clone()));
        remaining_need -= take;
        remaining_need = round(remaining_need);
    }

    if remaining_need > zero {
        let available = quantity_needed - &remaining_need;
        return Err(AccountingError::InsufficientLots {
            needed: quantity_needed.clone(),
            available: round(available),
        });
    }

    Ok(plan)
}

// ── Precision helper ──────────────────────────────────────────────────────────

/// Round `value` to 7 decimal places using ROUND_HALF_UP.
fn round(value: BigDecimal) -> BigDecimal {
    value.with_scale_round(SCALE as i64, RoundingMode::HalfUp)
}

// ── Accounting service ────────────────────────────────────────────────────────

/// Stateless accounting engine.
///
/// In production the `lots`, `consumptions`, and `pnl` slices are backed by
/// the `tax_lots`, `lot_consumptions`, and `realized_pnl` database tables.
/// The service operates on owned Rust values; persistence is the caller's
/// responsibility so the service remains unit-testable in isolation.
pub struct AccountingService;

impl AccountingService {
    /// Create a new [`TaxLot`] from a buy fill or incoming credit.
    ///
    /// `quantity` is the gross amount received; `cost_per_unit` is the
    /// reporting-currency price at acquisition time; `fee` is the acquisition
    /// fee in the reporting currency (may be zero).
    pub fn create_lot(
        tenant_id: Uuid,
        asset_code: impl Into<String>,
        asset_issuer: Option<String>,
        quantity: BigDecimal,
        cost_per_unit: BigDecimal,
        fee: BigDecimal,
        reporting_currency: impl Into<String>,
        source_order_id: impl Into<String>,
        acquired_at: DateTime<Utc>,
    ) -> Result<TaxLot, AccountingError> {
        let quantity = round(quantity);
        let cost_per_unit = round(cost_per_unit);
        let fee = round(fee);
        let total_cost = round(&quantity * &cost_per_unit);

        Ok(TaxLot {
            id: Uuid::new_v4(),
            tenant_id,
            asset_code: asset_code.into(),
            asset_issuer,
            quantity: quantity.clone(),
            remaining: quantity,
            cost_per_unit,
            total_cost,
            reporting_currency: reporting_currency.into(),
            acquisition_fee: fee,
            source_order_id: source_order_id.into(),
            acquired_at,
            reversed: false,
        })
    }

    /// Consume lots for a sell fill, using the specified `method`.
    ///
    /// Returns:
    /// - Updated lots (with `remaining` decremented),
    /// - [`LotConsumption`] records (one per lot touched),
    /// - A single [`RealizedPnl`] entry.
    pub fn consume_lots(
        lots: &mut Vec<TaxLot>,
        tenant_id: Uuid,
        asset_code: &str,
        asset_issuer: Option<&str>,
        quantity_sold: BigDecimal,
        proceeds: BigDecimal,
        sale_fee: BigDecimal,
        reporting_currency: &str,
        method: LotMethod,
        source_order_id: &str,
        sold_at: DateTime<Utc>,
        specific_ids: Option<&[Uuid]>,
    ) -> Result<(Vec<LotConsumption>, RealizedPnl), AccountingError> {
        let quantity_sold = round(quantity_sold);
        let proceeds = round(proceeds);
        let sale_fee = round(sale_fee);

        // Filter to available lots for this tenant + asset
        let eligible: Vec<TaxLot> = lots
            .iter()
            .filter(|l| {
                !l.reversed
                    && l.remaining > BigDecimal::from(0)
                    && l.tenant_id == tenant_id
                    && l.asset_code == asset_code
                    && l.asset_issuer.as_deref() == asset_issuer
            })
            .cloned()
            .collect();

        let selector: Box<dyn LotSelector> = match method {
            LotMethod::Fifo => Box::new(FifoSelector),
            LotMethod::Lifo => Box::new(LifoSelector),
            LotMethod::Hifo => Box::new(HifoSelector),
            LotMethod::SpecificLot => Box::new(SpecificLotSelector),
        };

        let plan = selector.select(&eligible, &quantity_sold, specific_ids)?;

        let mut consumptions = Vec::new();
        let mut total_cost_basis = BigDecimal::from(0);

        for (eligible_idx, qty) in &plan {
            let lot = &eligible[*eligible_idx];
            // Apply the consumption to the original lot in `lots`
            if let Some(orig) = lots.iter_mut().find(|l| l.id == lot.id) {
                orig.remaining = round(orig.remaining.clone() - qty);
            }

            let cost_basis = round(&lot.cost_per_unit * qty);
            total_cost_basis += &cost_basis;

            consumptions.push(LotConsumption {
                id: Uuid::new_v4(),
                lot_id: lot.id,
                tenant_id,
                quantity_consumed: qty.clone(),
                cost_basis,
                source_order_id: source_order_id.to_string(),
                consumed_at: sold_at,
                reversed: false,
            });
        }

        let total_cost_basis = round(total_cost_basis);
        let gain_loss = round(proceeds.clone() - &total_cost_basis - &sale_fee);
        let consumption_ids: Vec<Uuid> = consumptions.iter().map(|c| c.id).collect();

        let pnl = RealizedPnl {
            id: Uuid::new_v4(),
            tenant_id,
            asset_code: asset_code.to_string(),
            asset_issuer: asset_issuer.map(str::to_string),
            proceeds,
            cost_basis: total_cost_basis,
            gain_loss,
            sale_fee,
            lot_method: method,
            consumption_ids,
            reporting_currency: reporting_currency.to_string(),
            source_order_id: source_order_id.to_string(),
            realized_at: sold_at,
            reversed: false,
        };

        Ok((consumptions, pnl))
    }

    /// Reverse a lot (e.g. compensation or refund).
    ///
    /// Marks the lot as reversed and restores any quantities consumed by the
    /// given `order_id`.  Returns the modified lot and reversed consumption
    /// records.
    pub fn reverse_lot(
        lot: &mut TaxLot,
        consumptions: &mut Vec<LotConsumption>,
        order_id: &str,
    ) -> Result<(), AccountingError> {
        if lot.reversed {
            return Err(AccountingError::AlreadyReversed { lot_id: lot.id });
        }

        // Restore quantity for every un-reversed consumption from this order
        for c in consumptions.iter_mut().filter(|c| {
            c.lot_id == lot.id && c.source_order_id == order_id && !c.reversed
        }) {
            lot.remaining = round(lot.remaining.clone() + &c.quantity_consumed);
            c.reversed = true;
        }

        lot.reversed = true;
        Ok(())
    }

    /// Reverse a [`RealizedPnl`] entry (e.g. for a compensation flow).
    pub fn reverse_pnl(pnl: &mut RealizedPnl) -> Result<(), AccountingError> {
        if pnl.reversed {
            return Err(AccountingError::AlreadyReversed { lot_id: pnl.id });
        }
        pnl.reversed = true;
        Ok(())
    }

    /// Summarise realized P&L for a tenant over a period.
    pub fn summarize_pnl<'a>(
        entries: impl Iterator<Item = &'a RealizedPnl>,
        tenant_id: Uuid,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        reporting_currency: &str,
    ) -> PnlSummary {
        let mut total_proceeds = BigDecimal::from(0);
        let mut total_cost_basis = BigDecimal::from(0);
        let mut total_fees = BigDecimal::from(0);
        let mut total_gain_loss = BigDecimal::from(0);
        let mut entry_count = 0u64;

        for e in entries {
            if e.reversed
                || e.tenant_id != tenant_id
                || e.reporting_currency != reporting_currency
                || e.realized_at < from
                || e.realized_at > to
            {
                continue;
            }
            total_proceeds += &e.proceeds;
            total_cost_basis += &e.cost_basis;
            total_fees += &e.sale_fee;
            total_gain_loss += &e.gain_loss;
            entry_count += 1;
        }

        PnlSummary {
            tenant_id,
            from,
            to,
            reporting_currency: reporting_currency.to_string(),
            total_proceeds: round(total_proceeds),
            total_cost_basis: round(total_cost_basis),
            total_fees: round(total_fees),
            total_gain_loss: round(total_gain_loss),
            entry_count,
        }
    }
}

/// Aggregated P&L summary for a period.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PnlSummary {
    pub tenant_id: Uuid,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub reporting_currency: String,
    pub total_proceeds: BigDecimal,
    pub total_cost_basis: BigDecimal,
    pub total_fees: BigDecimal,
    /// `total_proceeds - total_cost_basis - total_fees`
    pub total_gain_loss: BigDecimal,
    pub entry_count: u64,
}

// ── Migration SQL ─────────────────────────────────────────────────────────────
// See migrations/20261022000000_tax_lots.sql (added in this PR).

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn usdc_lot(
        tenant: Uuid,
        qty: &str,
        cost: &str,
        acquired_at: DateTime<Utc>,
    ) -> TaxLot {
        AccountingService::create_lot(
            tenant,
            "USDC",
            Some("GISSUERA".into()),
            BigDecimal::from_str(qty).unwrap(),
            BigDecimal::from_str(cost).unwrap(),
            BigDecimal::from(0),
            "USD",
            format!("order-{}", Uuid::new_v4()),
            acquired_at,
        )
        .unwrap()
    }

    #[test]
    fn create_lot_rounds_to_7dp() {
        let lot = usdc_lot(
            Uuid::new_v4(),
            "10.123456789",
            "1.0",
            Utc::now(),
        );
        assert_eq!(
            lot.quantity.to_string(),
            "10.1234568" // ROUND_HALF_UP at 7dp
        );
    }

    #[test]
    fn fifo_consumes_oldest_first() {
        let tenant = Uuid::new_v4();
        let t1 = DateTime::from_timestamp(1_000_000, 0).unwrap();
        let t2 = DateTime::from_timestamp(2_000_000, 0).unwrap();

        let mut lots = vec![
            usdc_lot(tenant, "5.0", "1.0", t2), // newer
            usdc_lot(tenant, "5.0", "1.0", t1), // older
        ];

        let (consumptions, pnl) = AccountingService::consume_lots(
            &mut lots,
            tenant,
            "USDC",
            Some("GISSUERA"),
            BigDecimal::from_str("3.0").unwrap(),
            BigDecimal::from_str("3.5").unwrap(),
            BigDecimal::from(0),
            "USD",
            LotMethod::Fifo,
            "sell-order-1",
            Utc::now(),
            None,
        )
        .unwrap();

        // Should have consumed from the older lot (t1) first
        let consumed_lot_id = consumptions[0].lot_id;
        let older_lot = lots.iter().find(|l| l.acquired_at == t1).unwrap();
        assert_eq!(consumed_lot_id, older_lot.id);

        // Remaining on the older lot should be 5 - 3 = 2
        assert_eq!(older_lot.remaining, BigDecimal::from_str("2.0000000").unwrap());

        assert_eq!(pnl.gain_loss, BigDecimal::from_str("0.5000000").unwrap());
    }

    #[test]
    fn lifo_consumes_newest_first() {
        let tenant = Uuid::new_v4();
        let t1 = DateTime::from_timestamp(1_000_000, 0).unwrap();
        let t2 = DateTime::from_timestamp(2_000_000, 0).unwrap();

        let mut lots = vec![
            usdc_lot(tenant, "5.0", "1.0", t1), // older
            usdc_lot(tenant, "5.0", "2.0", t2), // newer, higher cost
        ];

        let (consumptions, _pnl) = AccountingService::consume_lots(
            &mut lots,
            tenant,
            "USDC",
            Some("GISSUERA"),
            BigDecimal::from_str("3.0").unwrap(),
            BigDecimal::from_str("6.0").unwrap(),
            BigDecimal::from(0),
            "USD",
            LotMethod::Lifo,
            "sell-order-2",
            Utc::now(),
            None,
        )
        .unwrap();

        let consumed_lot_id = consumptions[0].lot_id;
        let newer_lot = lots.iter().find(|l| l.acquired_at == t2).unwrap();
        assert_eq!(consumed_lot_id, newer_lot.id);
    }

    #[test]
    fn hifo_consumes_highest_cost_first() {
        let tenant = Uuid::new_v4();
        let t1 = DateTime::from_timestamp(1_000_000, 0).unwrap();
        let t2 = DateTime::from_timestamp(2_000_000, 0).unwrap();

        let mut lots = vec![
            usdc_lot(tenant, "5.0", "0.5", t1), // cheap
            usdc_lot(tenant, "5.0", "2.0", t2), // expensive
        ];

        let (consumptions, _pnl) = AccountingService::consume_lots(
            &mut lots,
            tenant,
            "USDC",
            Some("GISSUERA"),
            BigDecimal::from_str("3.0").unwrap(),
            BigDecimal::from_str("6.0").unwrap(),
            BigDecimal::from(0),
            "USD",
            LotMethod::Hifo,
            "sell-order-3",
            Utc::now(),
            None,
        )
        .unwrap();

        let consumed_lot_id = consumptions[0].lot_id;
        let expensive_lot = lots.iter().find(|l| l.cost_per_unit == BigDecimal::from_str("2.0").unwrap()).unwrap();
        assert_eq!(consumed_lot_id, expensive_lot.id);
    }

    #[test]
    fn partial_lot_consumption() {
        let tenant = Uuid::new_v4();
        let mut lots = vec![usdc_lot(tenant, "10.0", "1.0", Utc::now())];

        let (_, _) = AccountingService::consume_lots(
            &mut lots,
            tenant,
            "USDC",
            Some("GISSUERA"),
            BigDecimal::from_str("4.0").unwrap(),
            BigDecimal::from_str("4.0").unwrap(),
            BigDecimal::from(0),
            "USD",
            LotMethod::Fifo,
            "order-partial",
            Utc::now(),
            None,
        )
        .unwrap();

        assert_eq!(
            lots[0].remaining,
            BigDecimal::from_str("6.0000000").unwrap()
        );
        assert!(!lots[0].is_fully_consumed());
    }

    #[test]
    fn insufficient_lots_returns_error() {
        let tenant = Uuid::new_v4();
        let mut lots = vec![usdc_lot(tenant, "2.0", "1.0", Utc::now())];

        let result = AccountingService::consume_lots(
            &mut lots,
            tenant,
            "USDC",
            Some("GISSUERA"),
            BigDecimal::from_str("5.0").unwrap(),
            BigDecimal::from_str("5.0").unwrap(),
            BigDecimal::from(0),
            "USD",
            LotMethod::Fifo,
            "order-excess",
            Utc::now(),
            None,
        );

        assert!(matches!(result, Err(AccountingError::InsufficientLots { .. })));
    }

    #[test]
    fn specific_lot_selection() {
        let tenant = Uuid::new_v4();
        let t1 = DateTime::from_timestamp(1_000_000, 0).unwrap();
        let t2 = DateTime::from_timestamp(2_000_000, 0).unwrap();

        let mut lots = vec![
            usdc_lot(tenant, "5.0", "1.0", t1),
            usdc_lot(tenant, "5.0", "3.0", t2),
        ];
        let target_id = lots[1].id; // pick the expensive lot explicitly

        let (consumptions, _pnl) = AccountingService::consume_lots(
            &mut lots,
            tenant,
            "USDC",
            Some("GISSUERA"),
            BigDecimal::from_str("2.0").unwrap(),
            BigDecimal::from_str("6.0").unwrap(),
            BigDecimal::from(0),
            "USD",
            LotMethod::SpecificLot,
            "order-specific",
            Utc::now(),
            Some(&[target_id]),
        )
        .unwrap();

        assert_eq!(consumptions[0].lot_id, target_id);
    }

    #[test]
    fn specific_lot_without_ids_returns_error() {
        let tenant = Uuid::new_v4();
        let mut lots = vec![usdc_lot(tenant, "5.0", "1.0", Utc::now())];

        let result = AccountingService::consume_lots(
            &mut lots,
            tenant,
            "USDC",
            Some("GISSUERA"),
            BigDecimal::from_str("1.0").unwrap(),
            BigDecimal::from_str("1.0").unwrap(),
            BigDecimal::from(0),
            "USD",
            LotMethod::SpecificLot,
            "order-nospec",
            Utc::now(),
            None, // no IDs
        );

        assert!(matches!(
            result,
            Err(AccountingError::SpecificLotIdsRequired)
        ));
    }

    #[test]
    fn reverse_lot_restores_remaining() {
        let tenant = Uuid::new_v4();
        let order_id = "order-rev";
        let mut lots = vec![usdc_lot(tenant, "10.0", "1.0", Utc::now())];

        let (mut consumptions, _) = AccountingService::consume_lots(
            &mut lots,
            tenant,
            "USDC",
            Some("GISSUERA"),
            BigDecimal::from_str("4.0").unwrap(),
            BigDecimal::from_str("4.0").unwrap(),
            BigDecimal::from(0),
            "USD",
            LotMethod::Fifo,
            order_id,
            Utc::now(),
            None,
        )
        .unwrap();

        assert_eq!(lots[0].remaining, BigDecimal::from_str("6.0000000").unwrap());

        AccountingService::reverse_lot(&mut lots[0], &mut consumptions, order_id).unwrap();

        // After reversal, remaining should be restored to original quantity
        // (10 - 4 + 4 = 10)
        assert_eq!(lots[0].remaining, BigDecimal::from_str("10.0000000").unwrap());
        assert!(lots[0].reversed);
    }

    #[test]
    fn double_reverse_returns_error() {
        let tenant = Uuid::new_v4();
        let mut lots = vec![usdc_lot(tenant, "5.0", "1.0", Utc::now())];
        let mut consumptions = vec![];
        AccountingService::reverse_lot(&mut lots[0], &mut consumptions, "order-x").unwrap();
        let result =
            AccountingService::reverse_lot(&mut lots[0], &mut consumptions, "order-x");
        assert!(matches!(result, Err(AccountingError::AlreadyReversed { .. })));
    }

    #[test]
    fn pnl_summary_aggregates_correctly() {
        let tenant = Uuid::new_v4();
        let now = Utc::now();

        let entries = vec![
            RealizedPnl {
                id: Uuid::new_v4(),
                tenant_id: tenant,
                asset_code: "USDC".into(),
                asset_issuer: None,
                proceeds: BigDecimal::from_str("10.0").unwrap(),
                cost_basis: BigDecimal::from_str("8.0").unwrap(),
                gain_loss: BigDecimal::from_str("2.0").unwrap(),
                sale_fee: BigDecimal::from(0),
                lot_method: LotMethod::Fifo,
                consumption_ids: vec![],
                reporting_currency: "USD".into(),
                source_order_id: "o1".into(),
                realized_at: now,
                reversed: false,
            },
            RealizedPnl {
                id: Uuid::new_v4(),
                tenant_id: tenant,
                asset_code: "USDC".into(),
                asset_issuer: None,
                proceeds: BigDecimal::from_str("5.0").unwrap(),
                cost_basis: BigDecimal::from_str("6.0").unwrap(),
                gain_loss: BigDecimal::from_str("-1.0").unwrap(),
                sale_fee: BigDecimal::from(0),
                lot_method: LotMethod::Fifo,
                consumption_ids: vec![],
                reporting_currency: "USD".into(),
                source_order_id: "o2".into(),
                realized_at: now,
                reversed: false,
            },
        ];

        let from = DateTime::from_timestamp(0, 0).unwrap();
        let to = DateTime::from_timestamp(i64::MAX, 0).unwrap();

        let summary = AccountingService::summarize_pnl(
            entries.iter(),
            tenant,
            from,
            to,
            "USD",
        );

        assert_eq!(summary.total_proceeds, BigDecimal::from_str("15.0000000").unwrap());
        assert_eq!(summary.total_gain_loss, BigDecimal::from_str("1.0000000").unwrap());
        assert_eq!(summary.entry_count, 2);
    }

    /// Property test: total consumed quantity == fill quantity across an
    /// arbitrary multi-lot sequence.
    #[test]
    fn consumed_quantity_equals_fill_quantity() {
        let tenant = Uuid::new_v4();
        let t = Utc::now();
        let mut lots = vec![
            usdc_lot(tenant, "3.0", "1.0", t),
            usdc_lot(tenant, "4.0", "1.5", t),
            usdc_lot(tenant, "5.0", "2.0", t),
        ];

        let sell_qty = BigDecimal::from_str("9.0").unwrap();

        let (consumptions, _pnl) = AccountingService::consume_lots(
            &mut lots,
            tenant,
            "USDC",
            Some("GISSUERA"),
            sell_qty.clone(),
            BigDecimal::from_str("10.0").unwrap(),
            BigDecimal::from(0),
            "USD",
            LotMethod::Fifo,
            "order-prop",
            Utc::now(),
            None,
        )
        .unwrap();

        let total_consumed: BigDecimal = consumptions.iter().map(|c| c.quantity_consumed.clone()).sum();
        let total_consumed = round(total_consumed);
        let sell_qty = round(sell_qty);
        assert_eq!(total_consumed, sell_qty);
    }
}
