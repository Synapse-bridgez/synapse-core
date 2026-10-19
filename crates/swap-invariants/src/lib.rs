//! Standalone swap invariants library for testing and fuzzing.

use std::fmt;

/// Error returned by a SwapEngine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwapError {
    Unavailable,
    InvalidHandoff(String),
    Internal(String),
}

impl fmt::Display for SwapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SwapError::Unavailable => write!(f, "swap engine unavailable"),
            SwapError::InvalidHandoff(m) => write!(f, "invalid swap hand-off: {m}"),
            SwapError::Internal(m) => write!(f, "swap engine internal error: {m}"),
        }
    }
}

impl std::error::Error for SwapError {}

#[path = "../../../src/swap/invariants.rs"]
pub mod invariants;

#[path = "../../../src/swap/engine.rs"]
pub mod engine;

pub use engine::*;
pub use invariants::*;
