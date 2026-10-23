//! Concrete [`LiquiditySource`](crate::swap::quoting::LiquiditySource)
//! implementations.

pub mod http;

pub use http::HttpLiquiditySource;
