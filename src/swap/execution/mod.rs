//! Swap order execution engine.
//!
//! This module wires together the order state machine, risk/compliance
//! checks, and the balance reservation ledger that prevents concurrent
//! in-flight orders from over-committing the same funds.

pub mod reservation;

pub use reservation::{
    AvailableBalance, ReservationError, ReservationLedger, ReservationRecord, ReservationState,
};
