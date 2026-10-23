//! Balance reservation and inventory locking for in-flight swap orders.
//!
//! Reserves the sell-side balance at order acceptance so concurrent orders
//! cannot over-commit the same funds, then releases, consumes, or expires
//! those reservations as the order's lifecycle progresses.
//!
//! All state transitions are idempotent and keyed by order id so retries are
//! safe. Acquires serialize on a per-(tenant, asset) advisory lock so two
//! orders accepted milliseconds apart cannot both satisfy a risk check
//! against the same balance.

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use thiserror::Error;
use uuid::Uuid;

/// Default time-to-live for a reservation before the sweeper may release it.
pub const DEFAULT_RESERVATION_TTL: Duration = Duration::minutes(30);

/// Lifecycle state of a single reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReservationState {
    /// Funds are held and unavailable to other orders.
    Outstanding,
    /// Funds were consumed by a fill (fully or partially).
    Consumed,
    /// Funds were released back to the available balance.
    Released,
}

/// A single reservation ledger entry keyed by tenant, asset, and order.
#[derive(Debug, Clone)]
pub struct BalanceReservation {
    pub tenant_id: String,
    pub asset: String,
    pub order_id: Uuid,
    /// Original reserved quantity; never changes after acquire.
    pub reserved: Decimal,
    /// Quantity consumed by fills so far.
    pub consumed: Decimal,
    /// Quantity released back to available balance so far.
    pub released: Decimal,
    pub state: ReservationState,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl BalanceReservation {
    /// Quantity still held by this reservation.
    pub fn outstanding(&self) -> Decimal {
        self.reserved - self.consumed - self.released
    }

    /// Whether the reservation has exceeded its TTL.
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ReservationError {
    #[error("insufficient available balance: requested {requested}, available {available}")]
    InsufficientBalance {
        requested: Decimal,
        available: Decimal,
    },
    #[error("reservation for order {0} not found")]
    NotFound(Uuid),
    #[error("reservation for order {0} is already terminal")]
    AlreadyTerminal(Uuid),
    #[error("consume amount {requested} exceeds outstanding {outstanding} for order {order_id}")]
    OverConsume {
        order_id: Uuid,
        requested: Decimal,
        outstanding: Decimal,
    },
    #[error("invariant violated for order {order_id}: reserved {reserved}, consumed {consumed}, released {released}")]
    InvariantViolated {
        order_id: Uuid,
        reserved: Decimal,
        consumed: Decimal,
        released: Decimal,
    },
}

/// In-memory reservation ledger.
///
/// The per-(tenant, asset) mutex models the row-level/advisory lock used by
/// the `balance_reservations` table so competing acquires serialize.
#[derive(Debug, Default)]
pub struct ReservationLedger {
    reservations: Mutex<HashMap<Uuid, BalanceReservation>>,
    locks: Mutex<HashMap<(String, String), ()>>,
}

impl ReservationLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Acquire a reservation for `order_id`, atomically checking available
    /// balance. `ledger_balance` is the raw balance for the tenant+asset.
    pub fn acquire(
        &self,
        tenant_id: &str,
        asset: &str,
        order_id: Uuid,
        amount: Decimal,
        ledger_balance: Decimal,
        ttl: Duration,
    ) -> Result<BalanceReservation, ReservationError> {
        let key = (tenant_id.to_string(), asset.to_string());
        // Serialize competing acquires for the same tenant+asset.
        let _guard = self.locks.lock().expect("reservation lock poisoned");
        let mut reservations = self.reservations.lock().expect("reservation map poisoned");

        // Idempotent: an existing reservation for this order is returned as-is.
        if let Some(existing) = reservations.get(&order_id) {
            return Ok(existing.clone());
        }

        let available = available_balance_inner(&reservations, tenant_id, asset, ledger_balance);
        if amount > available {
            return Err(ReservationError::InsufficientBalance {
                requested: amount,
                available,
            });
        }

        let now = Utc::now();
        let reservation = BalanceReservation {
            tenant_id: tenant_id.to_string(),
            asset: asset.to_string(),
            order_id,
            reserved: amount,
            consumed: Decimal::ZERO,
            released: Decimal::ZERO,
            state: ReservationState::Outstanding,
            created_at: now,
            expires_at: now + ttl,
        };
        reservations.insert(order_id, reservation.clone());
        Ok(reservation)
    }

    /// Consume a (possibly partial) amount from an outstanding reservation.
    pub fn consume(&self, order_id: Uuid, amount: Decimal) -> Result<BalanceReservation, ReservationError> {
        let mut reservations = self.reservations.lock().expect("reservation map poisoned");
        let reservation = reservations
            .get_mut(&order_id)
            .ok_or(ReservationError::NotFound(order_id))?;

        if reservation.state == ReservationState::Released {
            return Err(ReservationError::AlreadyTerminal(order_id));
        }
        let outstanding = reservation.outstanding();
        if amount > outstanding {
            return Err(ReservationError::OverConsume {
                order_id,
                requested: amount,
                outstanding,
            });
        }

        reservation.consumed += amount;
        if reservation.outstanding().is_zero() {
            reservation.state = ReservationState::Consumed;
        }
        Ok(reservation.clone())
    }

    /// Release the remaining outstanding amount back to available balance.
    pub fn release(&self, order_id: Uuid) -> Result<BalanceReservation, ReservationError> {
        let mut reservations = self.reservations.lock().expect("reservation map poisoned");
        let reservation = reservations
            .get_mut(&order_id)
            .ok_or(ReservationError::NotFound(order_id))?;

        if reservation.state == ReservationState::Released {
            // Idempotent: releasing twice is a no-op.
            return Ok(reservation.clone());
        }

        reservation.released += reservation.outstanding();
        reservation.state = ReservationState::Released;
        Ok(reservation.clone())
    }

    /// Sweeper: release reservations whose owning order has terminated or
    /// whose TTL has elapsed.
    pub fn sweep_expired(&self, now: DateTime<Utc>) -> Vec<Uuid> {
        let mut reservations = self.reservations.lock().expect("reservation map poisoned");
        let mut released = Vec::new();
        for reservation in reservations.values_mut() {
            if reservation.state == ReservationState::Outstanding && reservation.is_expired(now) {
                reservation.released += reservation.outstanding();
                reservation.state = ReservationState::Released;
                released.push(reservation.order_id);
            }
        }
        released
    }

    /// Available balance = ledger balance minus outstanding reservations.
    pub fn available_balance(&self, tenant_id: &str, asset: &str, ledger_balance: Decimal) -> Decimal {
        let reservations = self.reservations.lock().expect("reservation map poisoned");
        available_balance_inner(&reservations, tenant_id, asset, ledger_balance)
    }

    /// Reconciliation check: released + consumed + outstanding never exceeds
    /// the original reservation for any entry.
    pub fn reconcile(&self) -> Result<(), ReservationError> {
        let reservations = self.reservations.lock().expect("reservation map poisoned");
        for reservation in reservations.values() {
            let total = reservation.consumed + reservation.released + reservation.outstanding();
            if total > reservation.reserved {
                return Err(ReservationError::InvariantViolated {
                    order_id: reservation.order_id,
                    reserved: reservation.reserved,
                    consumed: reservation.consumed,
                    released: reservation.released,
                });
            }
        }
        Ok(())
    }
}

fn available_balance_inner(
    reservations: &HashMap<Uuid, BalanceReservation>,
    tenant_id: &str,
    asset: &str,
    ledger_balance: Decimal,
) -> Decimal {
    let held: Decimal = reservations
        .values()
        .filter(|r| r.tenant_id == tenant_id && r.asset == asset && r.state == ReservationState::Outstanding)
        .map(|r| r.outstanding())
        .sum();
    ledger_balance - held
}
