//! Use cases: application business logic.
//! Orchestrates domain and ports.

pub mod process_deposit;

pub use process_deposit::{DepositInput, DepositOutput, ProcessDeposit};
