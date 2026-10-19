//! Phase 2 swap domain model: the swap-order lifecycle from `SwapReady` through
//! `Settled`, its transition guards, and the typed value objects (asset, asset
//! pair, amount, quote, fill) that downstream swap components depend on.
//!
//! This is pure domain logic: no I/O, no persistence, no quoting or execution.
//! Amounts are exact integers in the asset's smallest unit (`10^-scale`), so no
//! floating point is involved. Timestamps are `DateTime<Utc>`. The diagram this
//! module encodes lives in `STATE_MACHINE_DIAGRAMS.md` ("Swap Order State Machine").

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;
use uuid::Uuid;

/// Largest asset scale (fractional digits) supported. Keeps `10^scale` well inside `i128`.
pub const MAX_ASSET_SCALE: u32 = 18;

/// Longest asset code accepted (Stellar's alphanumeric-12 limit).
pub const MAX_ASSET_CODE_LEN: usize = 12;

/// Lifecycle state of a swap order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwapState {
    SwapReady,
    QuoteRequested,
    Quoted,
    OrderCreated,
    Executing,
    Executed,
    SettlementPending,
    Settled,
    Failed,
    Cancelled,
    Expired,
}

impl SwapState {
    /// Every state, in lifecycle order.
    pub const ALL: [SwapState; 11] = [
        SwapState::SwapReady,
        SwapState::QuoteRequested,
        SwapState::Quoted,
        SwapState::OrderCreated,
        SwapState::Executing,
        SwapState::Executed,
        SwapState::SettlementPending,
        SwapState::Settled,
        SwapState::Failed,
        SwapState::Cancelled,
        SwapState::Expired,
    ];

    /// Stable persistence/API name. Identical to the serde representation.
    pub fn as_str(self) -> &'static str {
        match self {
            SwapState::SwapReady => "swap_ready",
            SwapState::QuoteRequested => "quote_requested",
            SwapState::Quoted => "quoted",
            SwapState::OrderCreated => "order_created",
            SwapState::Executing => "executing",
            SwapState::Executed => "executed",
            SwapState::SettlementPending => "settlement_pending",
            SwapState::Settled => "settled",
            SwapState::Failed => "failed",
            SwapState::Cancelled => "cancelled",
            SwapState::Expired => "expired",
        }
    }

    /// States this state may legally move to. Exhaustive over states, so adding a
    /// variant is a compile error until its outgoing edges are listed here.
    pub fn allowed_next(self) -> &'static [SwapState] {
        use SwapState::*;
        match self {
            SwapReady => &[QuoteRequested, Cancelled, Failed],
            QuoteRequested => &[Quoted, Cancelled, Failed],
            Quoted => &[OrderCreated, Expired, Cancelled, Failed],
            OrderCreated => &[Executing, Cancelled, Failed],
            Executing => &[Executed, Failed],
            Executed => &[SettlementPending, Failed],
            SettlementPending => &[Settled, Failed],
            Settled | Failed | Cancelled | Expired => &[],
        }
    }

    /// True when no transition leaves this state.
    pub fn is_terminal(self) -> bool {
        self.allowed_next().is_empty()
    }

    /// Checks the guard table. Self-transitions and undeclared edges are errors.
    pub fn transition_to(self, to: SwapState) -> Result<SwapState, SwapOrderError> {
        if self == to {
            Err(SwapOrderError::SelfTransition(self))
        } else if self.allowed_next().contains(&to) {
            Ok(to)
        } else {
            Err(SwapOrderError::IllegalTransition { from: self, to })
        }
    }
}

impl fmt::Display for SwapState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for SwapState {
    type Err = SwapOrderError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        SwapState::ALL
            .into_iter()
            .find(|state| state.as_str() == s)
            .ok_or_else(|| SwapOrderError::UnknownState(s.to_string()))
    }
}

/// Errors from the swap domain model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwapOrderError {
    SelfTransition(SwapState),
    IllegalTransition { from: SwapState, to: SwapState },
    UnknownState(String),
    InvalidAssetCode(String),
    InvalidAssetScale(u32),
    SameAsset,
    NonPositiveAmount,
    InvalidAmount(String),
    OverPrecision { value: String, scale: u32 },
    AssetMismatch,
    PairMismatch,
    Overflow,
    QuoteRequired,
    QuoteExpired,
}

impl fmt::Display for SwapOrderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SwapOrderError::SelfTransition(s) => write!(f, "self-transition from {s} is not allowed"),
            SwapOrderError::IllegalTransition { from, to } => {
                write!(f, "illegal swap transition: {from} -> {to}")
            }
            SwapOrderError::UnknownState(s) => write!(f, "unknown swap state: {s}"),
            SwapOrderError::InvalidAssetCode(c) => write!(f, "invalid asset code: {c}"),
            SwapOrderError::InvalidAssetScale(s) => {
                write!(f, "asset scale {s} exceeds maximum of {MAX_ASSET_SCALE}")
            }
            SwapOrderError::SameAsset => write!(f, "asset pair must have two distinct assets"),
            SwapOrderError::NonPositiveAmount => write!(f, "amount must be greater than zero"),
            SwapOrderError::InvalidAmount(v) => write!(f, "invalid amount: {v}"),
            SwapOrderError::OverPrecision { value, scale } => {
                write!(f, "amount {value} exceeds asset scale {scale}")
            }
            SwapOrderError::AssetMismatch => write!(f, "asset does not match the pair"),
            SwapOrderError::PairMismatch => write!(f, "quote pair does not match order pair"),
            SwapOrderError::Overflow => write!(f, "amount arithmetic overflowed"),
            SwapOrderError::QuoteRequired => write!(f, "transition requires a quote"),
            SwapOrderError::QuoteExpired => write!(f, "quote has expired"),
        }
    }
}

impl std::error::Error for SwapOrderError {}

/// An asset code and its decimal scale (number of fractional digits).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "AssetFields", into = "AssetFields")]
pub struct Asset {
    code: String,
    scale: u32,
}

#[derive(Serialize, Deserialize)]
struct AssetFields {
    code: String,
    scale: u32,
}

impl Asset {
    /// Uppercase ASCII alphanumeric code, 1 to [`MAX_ASSET_CODE_LEN`] characters.
    pub fn new(code: &str, scale: u32) -> Result<Self, SwapOrderError> {
        let valid_code = (1..=MAX_ASSET_CODE_LEN).contains(&code.len())
            && code
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit());
        if !valid_code {
            return Err(SwapOrderError::InvalidAssetCode(code.to_string()));
        }
        if scale > MAX_ASSET_SCALE {
            return Err(SwapOrderError::InvalidAssetScale(scale));
        }
        Ok(Self {
            code: code.to_string(),
            scale,
        })
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn scale(&self) -> u32 {
        self.scale
    }
}

impl TryFrom<AssetFields> for Asset {
    type Error = SwapOrderError;

    fn try_from(fields: AssetFields) -> Result<Self, Self::Error> {
        Asset::new(&fields.code, fields.scale)
    }
}

impl From<Asset> for AssetFields {
    fn from(asset: Asset) -> Self {
        AssetFields {
            code: asset.code,
            scale: asset.scale,
        }
    }
}

/// An ordered base/quote asset pair. The two assets must be distinct.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "PairFields", into = "PairFields")]
pub struct AssetPair {
    base: Asset,
    quote: Asset,
}

#[derive(Serialize, Deserialize)]
struct PairFields {
    base: Asset,
    quote: Asset,
}

impl AssetPair {
    pub fn new(base: Asset, quote: Asset) -> Result<Self, SwapOrderError> {
        if base.code == quote.code {
            return Err(SwapOrderError::SameAsset);
        }
        Ok(Self { base, quote })
    }

    pub fn base(&self) -> &Asset {
        &self.base
    }

    pub fn quote(&self) -> &Asset {
        &self.quote
    }
}

impl TryFrom<PairFields> for AssetPair {
    type Error = SwapOrderError;

    fn try_from(fields: PairFields) -> Result<Self, Self::Error> {
        AssetPair::new(fields.base, fields.quote)
    }
}

impl From<AssetPair> for PairFields {
    fn from(pair: AssetPair) -> Self {
        PairFields {
            base: pair.base,
            quote: pair.quote,
        }
    }
}

/// A strictly positive quantity of one asset, stored as exact integer units of
/// the asset's smallest denomination (`10^-scale`). Serializes as a decimal string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "AmountFields", into = "AmountFields")]
pub struct Amount {
    units: i128,
    asset: Asset,
}

#[derive(Serialize, Deserialize)]
struct AmountFields {
    value: String,
    asset: Asset,
}

impl Amount {
    /// Parses a plain decimal string such as `"100.50"` at `asset`'s scale.
    /// Rejects zero, negative, malformed, over-precision, and overflowing values.
    pub fn parse(value: &str, asset: Asset) -> Result<Self, SwapOrderError> {
        if value.starts_with('-') {
            return Err(SwapOrderError::NonPositiveAmount);
        }
        let invalid = || SwapOrderError::InvalidAmount(value.to_string());
        let (whole, frac) = match value.split_once('.') {
            Some((whole, frac)) if !frac.is_empty() => (whole, frac),
            Some(_) => return Err(invalid()),
            None => (value, ""),
        };
        let digits_only = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
        if whole.is_empty() || !digits_only(whole) || !digits_only(frac) {
            return Err(invalid());
        }
        // Trailing zeros beyond the scale carry no value, so they are not over-precision.
        let frac = frac.trim_end_matches('0');
        let scale = asset.scale as usize;
        if frac.len() > scale {
            return Err(SwapOrderError::OverPrecision {
                value: value.to_string(),
                scale: asset.scale,
            });
        }
        let units: i128 = format!("{whole}{frac:0<scale$}")
            .parse()
            .map_err(|_| SwapOrderError::Overflow)?;
        Self::from_units(units, asset)
    }

    /// Builds an amount from raw smallest units. Rejects zero and negative values.
    pub fn from_units(units: i128, asset: Asset) -> Result<Self, SwapOrderError> {
        if units <= 0 {
            return Err(SwapOrderError::NonPositiveAmount);
        }
        Ok(Self { units, asset })
    }

    pub fn units(&self) -> i128 {
        self.units
    }

    pub fn asset(&self) -> &Asset {
        &self.asset
    }

    /// Adds two amounts of the same asset. Cross-asset and overflowing sums are errors.
    pub fn checked_add(&self, other: &Amount) -> Result<Amount, SwapOrderError> {
        self.ensure_same_asset(other)?;
        let units = self
            .units
            .checked_add(other.units)
            .ok_or(SwapOrderError::Overflow)?;
        Ok(Self {
            units,
            asset: self.asset.clone(),
        })
    }

    /// Compares two amounts of the same asset. Cross-asset comparison is an error.
    pub fn checked_cmp(&self, other: &Amount) -> Result<Ordering, SwapOrderError> {
        self.ensure_same_asset(other)?;
        Ok(self.units.cmp(&other.units))
    }

    fn ensure_same_asset(&self, other: &Amount) -> Result<(), SwapOrderError> {
        if self.asset == other.asset {
            Ok(())
        } else {
            Err(SwapOrderError::AssetMismatch)
        }
    }
}

impl fmt::Display for Amount {
    /// Always renders exactly `scale` fractional digits, e.g. `100.50` at scale 2.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let scale = self.asset.scale as usize;
        let digits = self.units.to_string();
        if scale == 0 {
            return write!(f, "{digits}");
        }
        let padded = format!("{digits:0>width$}", width = scale + 1);
        let (whole, frac) = padded.split_at(padded.len() - scale);
        write!(f, "{whole}.{frac}")
    }
}

impl TryFrom<AmountFields> for Amount {
    type Error = SwapOrderError;

    fn try_from(fields: AmountFields) -> Result<Self, Self::Error> {
        Amount::parse(&fields.value, fields.asset)
    }
}

impl From<Amount> for AmountFields {
    fn from(amount: Amount) -> Self {
        AmountFields {
            value: amount.to_string(),
            asset: amount.asset,
        }
    }
}

/// Checks that a trade sells the pair's base asset and buys its quote asset.
fn check_trade_assets(pair: &AssetPair, sell: &Amount, buy: &Amount) -> Result<(), SwapOrderError> {
    if sell.asset() == pair.base() && buy.asset() == pair.quote() {
        Ok(())
    } else {
        Err(SwapOrderError::AssetMismatch)
    }
}

/// A priced offer to sell `sell` for `buy` on `pair`, valid until `expires_at`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Quote {
    pair: AssetPair,
    sell: Amount,
    buy: Amount,
    expires_at: DateTime<Utc>,
}

impl Quote {
    pub fn new(
        pair: AssetPair,
        sell: Amount,
        buy: Amount,
        expires_at: DateTime<Utc>,
    ) -> Result<Self, SwapOrderError> {
        check_trade_assets(&pair, &sell, &buy)?;
        Ok(Self {
            pair,
            sell,
            buy,
            expires_at,
        })
    }

    /// A quote is expired at and after its expiry instant.
    pub fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }

    pub fn pair(&self) -> &AssetPair {
        &self.pair
    }

    pub fn sell(&self) -> &Amount {
        &self.sell
    }

    pub fn buy(&self) -> &Amount {
        &self.buy
    }

    pub fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }
}

/// An executed trade of `sell` base for `buy` quote at `executed_at`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Fill {
    sell: Amount,
    buy: Amount,
    executed_at: DateTime<Utc>,
}

impl Fill {
    pub fn new(
        pair: &AssetPair,
        sell: Amount,
        buy: Amount,
        executed_at: DateTime<Utc>,
    ) -> Result<Self, SwapOrderError> {
        check_trade_assets(pair, &sell, &buy)?;
        Ok(Self {
            sell,
            buy,
            executed_at,
        })
    }

    pub fn sell(&self) -> &Amount {
        &self.sell
    }

    pub fn buy(&self) -> &Amount {
        &self.buy
    }

    pub fn executed_at(&self) -> DateTime<Utc> {
        self.executed_at
    }
}

/// A swap order: the base amount to sell on `pair`, its current lifecycle state,
/// and the quote it was priced against. Timestamps and actor/source metadata are
/// fields on the order; the transition logic only supplies them.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SwapOrder {
    id: Uuid,
    pair: AssetPair,
    amount: Amount,
    state: SwapState,
    quote: Option<Quote>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    /// Who last moved the order (user, service, or system). Set by the caller.
    actor: Option<String>,
    /// Originating system or flow that created the order.
    source: Option<String>,
}

impl SwapOrder {
    /// Creates an order in `SwapReady`. `amount` must be denominated in the pair's base asset.
    pub fn new(
        id: Uuid,
        pair: AssetPair,
        amount: Amount,
        source: Option<String>,
        at: DateTime<Utc>,
    ) -> Result<Self, SwapOrderError> {
        if amount.asset() != pair.base() {
            return Err(SwapOrderError::AssetMismatch);
        }
        Ok(Self {
            id,
            pair,
            amount,
            state: SwapState::SwapReady,
            quote: None,
            created_at: at,
            updated_at: at,
            actor: None,
            source,
        })
    }

    /// Attaches a quote and moves the order to `Quoted`. Rejects a quote for another
    /// pair, or one already expired at `at`, so a stale quote is never recorded.
    pub fn record_quote(
        &mut self,
        quote: Quote,
        at: DateTime<Utc>,
        actor: Option<String>,
    ) -> Result<(), SwapOrderError> {
        let next = self.state.transition_to(SwapState::Quoted)?;
        if quote.pair() != &self.pair {
            return Err(SwapOrderError::PairMismatch);
        }
        if quote.is_expired_at(at) {
            return Err(SwapOrderError::QuoteExpired);
        }
        self.quote = Some(quote);
        self.apply(next, at, actor);
        Ok(())
    }

    /// Moves the order along a legal edge.
    ///
    /// Guards: `Quoted` must be entered via [`SwapOrder::record_quote`], and
    /// `OrderCreated` requires an unexpired quote at `at`. An expired quote is an
    /// error and leaves the state unchanged. The caller may then move the order
    /// forward to `Expired`; it can never move backwards.
    pub fn transition_to(
        &mut self,
        to: SwapState,
        at: DateTime<Utc>,
        actor: Option<String>,
    ) -> Result<(), SwapOrderError> {
        let next = self.state.transition_to(to)?;
        if next == SwapState::Quoted {
            return Err(SwapOrderError::QuoteRequired);
        }
        if next == SwapState::OrderCreated {
            let quote = self.quote.as_ref().ok_or(SwapOrderError::QuoteRequired)?;
            if quote.is_expired_at(at) {
                return Err(SwapOrderError::QuoteExpired);
            }
        }
        self.apply(next, at, actor);
        Ok(())
    }

    fn apply(&mut self, state: SwapState, at: DateTime<Utc>, actor: Option<String>) {
        self.state = state;
        self.updated_at = at;
        self.actor = actor;
    }

    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn pair(&self) -> &AssetPair {
        &self.pair
    }

    pub fn amount(&self) -> &Amount {
        &self.amount
    }

    pub fn state(&self) -> SwapState {
        self.state
    }

    pub fn quote(&self) -> Option<&Quote> {
        self.quote.as_ref()
    }

    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    pub fn updated_at(&self) -> DateTime<Utc> {
        self.updated_at
    }

    pub fn actor(&self) -> Option<&str> {
        self.actor.as_deref()
    }

    pub fn source(&self) -> Option<&str> {
        self.source.as_deref()
    }
}
